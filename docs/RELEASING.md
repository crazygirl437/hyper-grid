# How to publish a release (GitHub Actions)

You do **not** need to build installers on your PC.

## 1. Create the GitHub repository (once)

On github.com: **New repository** → name e.g. `hyper-grid` → create (empty is fine).

Then on your machine:

```bash
cd /path/to/hyper-grid
git remote add origin git@github.com:YOUR_USER/hyper-grid.git
# or: https://github.com/YOUR_USER/hyper-grid.git

git push -u origin master
# if your default branch is main:
# git branch -M main && git push -u origin main
```

## 2. Ship a version

```bash
git add -A
git status
git commit -m "your message"
git push

git tag v0.1.0
git push origin v0.1.0
```

## 3. Download

GitHub → **Actions** (wait until green) → **Releases** → download:

- Windows `.exe`
- macOS `.dmg`
- Linux `.AppImage` / `.deb`
- `hyper-grid-cli-linux-x86_64` (static CLI)

## Manual run

**Actions → release → Run workflow** → set tag to `v0.1.0`.
