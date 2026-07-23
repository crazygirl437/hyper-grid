# Architecture (short)

- **Desktop**: Tauri 2 + React UI  
- **Engine**: Rust grid logic (`grid-engine`)  
- **Exchange**: Hyperliquid REST + local simulation (`exchange`)  
- **Storage**: local SQLite / `.env` config (`storage`)  

Packaging: GitHub Actions (`.github/workflows/release.yml`) builds Linux / Windows / macOS on tag `v*`.
