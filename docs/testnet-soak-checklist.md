# Testnet soak checklist (pre-mainnet)

Run on Hyperliquid **testnet** for at least **24 hours** before enabling mainnet auto-run.

## Setup

- [ ] `MODE=testnet`, dynamic grid enabled, `EXIT_POLICY=preserve`, `RESUME_ON_RESTART=true`
- [ ] Modest budget and leverage; confirm UI shows activity band + ATR after start
- [ ] Note wallet address, symbol, session id from dashboard/events

## During soak

- [ ] Kill the desktop process at least twice; reopen and confirm resume without duplicate orders
- [ ] Disconnect network briefly; confirm `Recovering` then return to `Running`
- [ ] Wait for ≥2 automatic recenters (or force soft breakout via volatile symbol)
- [ ] Close the window (do not use Stop); confirm exchange still has orders/position
- [ ] Reopen; confirm fills during offline appear once in ledger (no duplicates)

## Stop / halt paths

- [ ] Click **停止（撤单并平仓）** with confirm; only strategy symbol cleared
- [ ] Trigger a hard risk halt (tight drawdown) once; confirm persistent banner and no auto-restart

## Consistency checks

- [ ] Exchange open orders == UI resting orders for strategy symbol
- [ ] Exchange position == UI position
- [ ] SQLite `session_checkpoints` phase matches UI status
- [ ] `fills` / `funding_payments` / `equity_snapshots` rows present for the session

## PnL analytics

- [ ] After a few fills, **收益分析** shows gross / fees / funding / net realized matching ledger
- [ ] Equity curve grows with snapshots (~30s); toggle mark vs closed modes
- [ ] Session table lists active session; click switches summary/curve
- [ ] Daily table shows local-date rows for 7d/30d
- [ ] **导出分析包** writes `fills.csv` / `funding.csv` / `equity.csv` / `summary.json` under app data `analytics/`

Only after all boxes pass, enable mainnet auto-run.
