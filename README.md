# hyper-grid

**English** | [中文](README.zh-CN.md)

An easy desktop app for **grid trading** on [Hyperliquid](https://hyperliquid.xyz) perpetual futures.

You set a price range and size. The app places buys below the market and sells above it — aiming to profit when price oscillates inside that range.

> **Recommended:** download a ready-made build from Releases. You do **not** need to compile on your laptop.
>
> **Maintainers:** package with **GitHub Actions** (not your local PC) — push a `v*` tag and installers appear on the Releases page.

---

## Demo video

<!-- Replace with your video link or embed. -->

**[▶ Watch demo](VIDEO_URL_HERE)**

---

## Download & run (recommended)

1. Open **[Releases](../../releases)** on GitHub.
2. Download for your OS:
   - **Windows** → `.exe` installer  
   - **macOS** → `.dmg`  
   - **Linux desktop** → `.AppImage` or `.deb` (**built on Ubuntu 22.04** for broader glibc compatibility)  
   - **Linux CLI (most portable)** → `hyper-grid-cli-linux-x86_64` (static musl binary — runs on almost any x86_64 Linux)
3. Install / open, then start the app.

**Linux tip:** GUI installers on Releases are built on **Ubuntu 22.04** via GitHub Actions. Prefer those over a binary compiled on a newer desktop OS.

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

When Actions finishes, open **Releases** and download the installers.

---

## More help

- [User guide (EN)](docs/USER_GUIDE.en.md)
- [用户指南（中文）](docs/USER_GUIDE.zh.md)

---

## License

MIT
