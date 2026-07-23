# 如何用 GitHub 发布

不需要在自己电脑上打包。

## 发一版

```bash
git add -A
git commit -m "说明这次改动"
git push

git tag v0.1.1
git push origin v0.1.1
```

## 会产出什么

只有桌面便携版（无安装包、无 CLI）：

- `hyper-grid-linux-x86_64.AppImage` — Linux（先 `chmod +x` 再运行）
- `hyper-grid-windows-x64.exe` — Windows（双击）
- `hyper-grid-macos-arm64.app.tar.gz` — 苹果芯片 Mac
- `hyper-grid-macos-x64.app.tar.gz` — Intel Mac

## 手动触发

**Actions → release → Run workflow** → 填写标签如 `v0.1.1`。
