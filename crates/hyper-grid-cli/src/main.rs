use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use exchange::{Exchange, HyperliquidExchange, SimExchange};
use grid_engine::{
    preview_grid, BreakoutAction, GridConfig, GridEngine, GridSpacing, RunMode,
};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use storage::Storage;
use tracing::info;

#[derive(Parser)]
#[command(name = "hyper-grid-cli", about = "hyper-grid command line")]
struct Cli {
    #[command(subcommand)]
    cmd: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Preview arithmetic/geometric grid levels
    Preview {
        #[arg(long, default_value = "BTC")]
        symbol: String,
        #[arg(long, default_value = "90000")]
        lower: String,
        #[arg(long, default_value = "100000")]
        upper: String,
        #[arg(long, default_value_t = 10)]
        levels: u32,
        #[arg(long, default_value = "1000")]
        budget: String,
        #[arg(long, default_value = "95000")]
        mid: String,
        #[arg(long, value_enum, default_value_t = SpacingArg::Arithmetic)]
        spacing: SpacingArg,
    },
    /// Run local simulation for N ticks
    Sim {
        #[arg(long, default_value_t = 30)]
        ticks: u32,
        #[arg(long, default_value = "BTC")]
        symbol: String,
        #[arg(long, default_value = "90000")]
        lower: String,
        #[arg(long, default_value = "100000")]
        upper: String,
        #[arg(long, default_value_t = 8)]
        levels: u32,
        #[arg(long, default_value = "1000")]
        budget: String,
    },
    /// Fetch mid price from Hyperliquid (testnet/mainnet)
    Mid {
        #[arg(long, default_value = "BTC")]
        symbol: String,
        #[arg(long, value_enum, default_value_t = ModeArg::Testnet)]
        mode: ModeArg,
    },
    /// Export fills CSV from local storage
    ExportFills {
        #[arg(long)]
        path: PathBuf,
    },
}

#[derive(Clone, ValueEnum)]
enum SpacingArg {
    Arithmetic,
    Geometric,
}

#[derive(Clone, ValueEnum)]
enum ModeArg {
    Testnet,
    Mainnet,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .init();
    let cli = Cli::parse();
    match cli.cmd {
        Commands::Preview {
            symbol,
            lower,
            upper,
            levels,
            budget,
            mid,
            spacing,
        } => {
            let cfg = GridConfig {
                symbol,
                lower_price: lower.parse()?,
                upper_price: upper.parse()?,
                grid_count: levels,
                total_budget: budget.parse()?,
                spacing: match spacing {
                    SpacingArg::Arithmetic => GridSpacing::Arithmetic,
                    SpacingArg::Geometric => GridSpacing::Geometric,
                },
                breakout_action: BreakoutAction::Pause,
                max_drawdown_pct: dec!(20),
                max_daily_loss: dec!(100),
                max_order_failures: 5,
                market: grid_engine::MarketKind::Perp,
                leverage: 5,
                is_cross: true,
            grid_mode: grid_engine::GridMode::Fixed,
            dynamic: grid_engine::DynamicGridConfig::default(),
            };
            let preview = preview_grid(&cfg, mid.parse()?)?;
            println!("{}", serde_json::to_string_pretty(&preview)?);
        }
        Commands::Sim {
            ticks,
            symbol,
            lower,
            upper,
            levels,
            budget,
        } => {
            let tmp = tempfile::tempdir()?;
            let storage = Storage::open(tmp.path())?;
            let cfg = GridConfig {
                symbol: symbol.clone(),
                lower_price: lower.parse()?,
                upper_price: upper.parse()?,
                grid_count: levels,
                total_budget: budget.parse()?,
                spacing: GridSpacing::Arithmetic,
                breakout_action: BreakoutAction::Pause,
                max_drawdown_pct: dec!(50),
                max_daily_loss: dec!(500),
                max_order_failures: 10,
                market: grid_engine::MarketKind::Perp,
                leverage: 5,
                is_cross: true,
            grid_mode: grid_engine::GridMode::Fixed,
            dynamic: grid_engine::DynamicGridConfig::default(),
            };
            let mid0 = (cfg.lower_price + cfg.upper_price) / Decimal::from(2);
            let mut sim = SimExchange::with_band(
                &symbol,
                mid0,
                cfg.total_budget * Decimal::from(3),
                Decimal::ZERO,
                cfg.lower_price,
                cfg.upper_price,
            );
            sim.connect().await?;
            let mut engine = GridEngine::new(cfg.clone(), RunMode::Simulation, cfg.total_budget)?;
            let intents = engine.bootstrap_intents(mid0)?;
            for order in sim.place_orders(intents).await? {
                engine.register_live_order(order);
            }
            for i in 0..ticks {
                let mid = sim.get_mid(&symbol).await?;
                let _ = engine.on_mid_price(mid);
                for fill in sim.drain_fills().await? {
                    let side = format!("{:?}", fill.side);
                    if let Ok((pnl, replenish)) = engine.on_fill(fill.clone()) {
                        storage.record_fill(
                            &fill.symbol,
                            &side,
                            fill.price,
                            fill.size,
                            pnl,
                            &fill.client_id,
                        )?;
                        if let Some(intent) = replenish {
                            let order = sim.place_order(intent).await?;
                            engine.register_live_order(order);
                        }
                    }
                }
                if i % 5 == 0 {
                    let snap = engine.snapshot();
                    info!(
                        "tick={i} mid={mid} orders={} pnl={}",
                        snap.open_orders, snap.realized_pnl
                    );
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            let snap = engine.snapshot();
            println!("{}", serde_json::to_string_pretty(&snap)?);
            let csv = tmp.path().join("fills.csv");
            let n = storage.export_fills_csv(&csv)?;
            info!("exported {n} fills to {}", csv.display());
        }
        Commands::Mid { symbol, mode } => {
            let mode = match mode {
                ModeArg::Testnet => RunMode::Testnet,
                ModeArg::Mainnet => RunMode::Mainnet,
            };
            let mut hl = HyperliquidExchange::new(mode);
            hl.connect().await?;
            let mid = hl.get_mid(&symbol).await?;
            println!("{symbol} mid = {mid}");
        }
        Commands::ExportFills { path } => {
            let storage = Storage::open_default()?;
            let n = storage.export_fills_csv(&path)?;
            println!("exported {n} rows to {}", path.display());
        }
    }
    Ok(())
}
