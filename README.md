# hyper-grid

**English** | [中文](README.zh-CN.md)

An easy desktop app for **grid trading** on [Hyperliquid](https://hyperliquid.xyz) perpetual futures.

You set a price range and size. The app places buys below the market and sells above it — aiming to profit when price oscillates inside that range.

> **Recommended:** download a ready-made build from Releases. You do **not** need to compile on your laptop.
>
> **Maintainers:** package with **GitHub Actions** — push a `v*` tag; portable desktop apps appear on the Releases page.

---

## Demo video

<!-- Replace with your video link or embed. -->

**[▶ Watch demo](VIDEO_URL_HERE)**

---

## Download & run (recommended)

1. Open **[Releases](../../releases)** on GitHub.
2. Download the portable app for your OS (no installer):
   - **Windows** → `hyper-grid-windows-x64.exe` (double-click)
   - **macOS Apple Silicon** → `hyper-grid-macos-arm64.app.tar.gz` (extract, open the `.app`)
   - **macOS Intel** → `hyper-grid-macos-x64.app.tar.gz` (extract, open the `.app`)
   - **Linux** → `hyper-grid-linux-x86_64.AppImage` — then:
     ```bash
     chmod +x hyper-grid-linux-x86_64.AppImage
     ./hyper-grid-linux-x86_64.AppImage
     ```
3. Start trading (try **Simulation** first).

**Linux note:** AppImage is built on **Ubuntu 22.04** (needs a recent glibc). Ubuntu **20.04** is not supported for the GUI build.

---

## What you need before live trading

1. Hyperliquid funds in the **perpetuals** account  
   ([Mainnet](https://app.hyperliquid.xyz) · [Testnet faucet](https://app.hyperliquid-testnet.xyz/drip))
2. That wallet’s **private key** (stored only on your machine)

This app does **not** deposit or withdraw for you.

---

## Quick start

1. Open hyper-grid → try **Simulation** first (no real money).
2. **Configure** → set symbol / range / grids / size → **Preview** → **Start**.
3. For real trading: **Testnet** or **Mainnet**, paste key, refresh balances, then start.

Language: **English / 中文** in Settings.

---

## Safety

- You can lose money — especially with high leverage.
- Never share your private key.
- Practice on Simulation / Testnet first; start small on mainnet.
- Stop cancels orders and closes positions.

---

## How maintainers ship builds (GitHub)

See **[docs/RELEASING.md](docs/RELEASING.md)** for the full steps.

Short version:

```bash
git push
git tag v0.1.0
git push origin v0.1.0
```

When Actions finishes, open **Releases** and download the portable apps.

---

## More help

- [User guide (EN)](docs/USER_GUIDE.en.md)
- [用户指南（中文）](docs/USER_GUIDE.zh.md)

---

## License

MIT
