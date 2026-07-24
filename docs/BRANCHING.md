# Dual-branch workflow

Two lines:

| Branch | Role |
|--------|------|
| **`main`** | Standard build — **all shared features, fixes, docs, CI** |
| **`extended`** | Standard + extension-only commits |

Baseline mapping:

- `main` → `c6b7497`
- `extended` → `e8679b4` (one commit ahead with extension logic)

## Shared work → `main`

```bash
git checkout main
# edit, commit, push
git push origin main
```

## Sync into extended

```bash
git checkout extended
git merge main
git push origin extended
```

## Extension-only work → `extended` only

Never land extension-only code on `main`.

## Releases

- Standard: tag on `main` (`v0.1.2`)
- Extended: merge `main` first, tag on `extended` (`v0.1.2-ext`)

## First push after restructure

```bash
git push -u origin main --force-with-lease
git push -u origin extended
```
