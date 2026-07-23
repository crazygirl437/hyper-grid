# How to publish a release (GitHub Actions)

You do **not** need to build on your PC.

## Ship a version

```bash
git add -A
git commit -m "your message"
git push

git tag v0.1.1
git push origin v0.1.1
```

## What you get

Portable desktop apps only (no installers, no CLI):

- `hyper-grid-linux-x86_64.AppImage` — Linux x64 (`chmod +x` then run)
- `hyper-grid-windows-x64.exe` — Windows x64 (double-click)
- `hyper-grid-macos-arm64.app.tar.gz` — Apple Silicon
- `hyper-grid-macos-x64.app.tar.gz` — Intel Mac

## Manual run

**Actions → release → Run workflow** → set tag e.g. `v0.1.1`.
