# 双版本分支说明

仓库维护两条线：

| 分支 | 提交基线 | 用途 |
|------|----------|------|
| **`main`** | 标准版（无扩展逻辑） | **通用功能、Bug 修复、文档、打包流程** 都在这里改 |
| **`extended`** | 在 `main` 之上多一段扩展提交 | 仅放 **扩展版独有** 的改动 |

当前对应关系：

- `main` → `c6b7497`（便携桌面打包等通用能力）
- `extended` → `e8679b4`（在标准版基础上追加扩展功能）

## 日常怎么改

### 改大家都需要的功能（网格、UI、交易所、打包…）

```bash
git checkout main
# …改代码…
git commit -m "fix: …"
git push origin main
```

### 把通用改动同步到扩展版

```bash
git checkout extended
git merge main
# 若有冲突：保留 extended 里扩展相关的代码，其余用 main 的
git push origin extended
```

### 只改扩展版才有的东西

```bash
git checkout extended
# …只改扩展逻辑…
git commit -m "feat(extended): …"
git push origin extended
```

**不要**在 `main` 上提交扩展版专属代码，否则两条线会缠在一起。

## 发版

- 标准版 Release：在 **`main`** 打 tag，例如 `v0.1.2`
- 扩展版 Release：在 **`extended`** 打 tag，例如 `v0.1.2-ext` 或 `v0.1.2-plus`

```bash
# 标准版
git checkout main && git tag v0.1.2 && git push origin v0.1.2

# 扩展版（先 merge main 再 tag）
git checkout extended && git merge main && git tag v0.1.2-ext && git push origin v0.1.2-ext
```

## 首次推到 GitHub

```bash
git push -u origin main --force-with-lease   # 若远端 main 还在 e8679b4，需要覆盖回标准版
git push -u origin extended
```

`--force-with-lease` 仅在你确认要把远端 `main` 从扩展版改回标准版时使用。
