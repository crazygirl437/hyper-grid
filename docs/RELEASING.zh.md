# 如何用 GitHub 发布安装包

不需要在自己电脑上打包。

## 1. 创建仓库（只需一次）

打开 github.com → **New repository** → 例如命名 `hyper-grid` → 创建（可先空仓库）。

本机执行：

```bash
cd /path/to/hyper-grid
git remote add origin git@github.com:你的用户名/hyper-grid.git
# 或: https://github.com/你的用户名/hyper-grid.git

git push -u origin master
# 若默认分支是 main：
# git branch -M main && git push -u origin main
```

## 2. 发一版

```bash
git add -A
git status
git commit -m "说明这次改动"
git push

git tag v0.1.0
git push origin v0.1.0
```

## 3. 下载

GitHub → **Actions**（等全部变绿）→ **Releases** → 下载：

- Windows `.exe`
- macOS `.dmg`
- Linux `.AppImage` / `.deb`
- `hyper-grid-cli-linux-x86_64`（静态命令行）

## 手动触发

**Actions → release → Run workflow** → 填写标签如 `v0.1.0`。
