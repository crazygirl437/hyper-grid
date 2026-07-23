# hyper-grid

[English](README.md) | **中文**

在 [Hyperliquid](https://hyperliquid.xyz) 上做**永续合约网格**的桌面软件。

设定价格区间和投入后，程序会在现价下方挂买、上方挂卖，希望行情在区间内波动时赚取差价。

> **推荐：** 直接下载 GitHub Releases 里已经编译好的安装包，**不必在本机编译**。
>
> **维护者：** 请用 **GitHub Actions** 打包（不要在自己电脑上硬编）——推送 `v*` 标签后，安装包会出现在 Releases 页面。

---

## 演示视频

<!-- 换成你的视频链接。 -->

**[▶ 观看演示](VIDEO_URL_HERE)**

---

## 下载安装（推荐）

1. 打开 **[Releases（发布页）](../../releases)**。
2. 按系统下载：
   - **Windows** → `.exe`  
   - **macOS** → `.dmg`  
   - **Linux 桌面版** → `.AppImage` / `.deb`（由 **Ubuntu 22.04** 构建，兼容性更好）  
   - **Linux 命令行（兼容性最好）** → `hyper-grid-cli-linux-x86_64`（musl 静态包）
3. 安装或打开即可。

**Linux 提示：** Releases 里的桌面安装包由 GitHub Actions 在 Ubuntu 22.04 上构建。请优先用它们，而不是在新系统本机自己编译的包。

---

## 实盘前需要准备

1. Hyperliquid **永续账户**里有资金  
   （[主网](https://app.hyperliquid.xyz) · [测试网领水](https://app.hyperliquid-testnet.xyz/drip)）
2. 该钱包**私钥**（只保存在本机）

软件**不会**帮你充值或提现。

---

## 三步上手

1. 先开 **模拟盘** 练习。  
2. **配置网格** → 预览 → 启动。  
3. 实盘：选测试网/主网，填私钥，刷新余额后再启动。

设置里可切换 **中文 / English**。

---

## 安全提醒

- 可能亏损，杠杆越高风险越大。  
- 私钥不要外传。  
- 先模拟/测试网，主网从小资金开始。  
- 停止时会撤单并平仓。

---

## 维护者如何用 GitHub 打包

完整步骤见 **[docs/RELEASING.zh.md](docs/RELEASING.zh.md)**。

简版：

```bash
git push
git tag v0.1.0
git push origin v0.1.0
```

等 Actions 跑完后，到 **Releases** 下载安装包。

---

## 更多说明

- [用户指南（中文）](docs/USER_GUIDE.zh.md)
- [User guide (EN)](docs/USER_GUIDE.en.md)

---

## 开源协议

MIT
