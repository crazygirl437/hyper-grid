# hyper-grid

[English](README.md) | **中文**

在 [Hyperliquid](https://hyperliquid.xyz) 上做**永续合约网格**的桌面软件。

设定价格区间和投入后，程序会在现价下方挂买、上方挂卖，希望行情在区间内波动时赚取差价。

> **推荐：** 直接下载 GitHub Releases 里的便携版，**不必在本机编译**。
>
> **维护者：** 请用 **GitHub Actions** 打包——推送 `v*` 标签后，可直接运行的桌面程序会出现在 Releases。

---

## 演示视频

<!-- 换成你的视频链接。 -->

**[▶ 观看演示](VIDEO_URL_HERE)**

---

## 下载运行（推荐）

1. 打开 **[Releases（发布页）](../../releases)**。
2. 按系统下载**便携版**（不是安装包）：
   - **Windows** → `hyper-grid-windows-x64.exe`（双击运行）
   - **macOS 苹果芯片** → `hyper-grid-macos-arm64.app.tar.gz`（解压后打开 `.app`）
   - **macOS Intel** → `hyper-grid-macos-x64.app.tar.gz`（解压后打开 `.app`）
   - **Linux** → `hyper-grid-linux-x86_64.AppImage`，然后：
     ```bash
     chmod +x hyper-grid-linux-x86_64.AppImage
     ./hyper-grid-linux-x86_64.AppImage
     ```
3. 建议先用 **模拟盘** 试跑。

**Linux 说明：** AppImage 在 **Ubuntu 22.04** 上构建，需要较新的 glibc；**Ubuntu 20.04 跑不了**桌面版。

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

等 Actions 跑完后，到 **Releases** 下载便携程序。

---

## 更多说明

- [用户指南（中文）](docs/USER_GUIDE.zh.md)
- [User guide (EN)](docs/USER_GUIDE.en.md)

---

## 开源协议

MIT
