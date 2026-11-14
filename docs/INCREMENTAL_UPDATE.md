# 增量升级：协议、发布与部署

本文说明正常系统端增量升级的完整链路。开发约束以 `AGENTS.md`“增量升级与待发布更新日志”为准。

## 数据流

```text
Release（main）
  最终 pkg/ ──release_update_tool generate──► pkg/bin/release-manifest.json（随 7z / 安装器分发）
                                         └──► $RUNNER_TEMP/update-objects/<sha256>（每种内容一份）
  publish-update-objects.ps1  ──► R2: LetRecovery/update/objects/<sha256>（不可变，已存在则跳过）
  publish-update-manifest.ps1 ──► update.php（letrecovery_app，HMAC）──► letrecovery.cloud-pe.cn/update/index.json
  发布全部成功 ──► 从最新 main 精确删除已发布的 PENDING_CHANGELOG.json 条目（[skip ci]）

客户端（正常系统端，主窗口创建后后台执行）
  GET https://letrecovery.cloud-pe.cn/update/index.json
  build_epoch > 本程序 BUILD_EPOCH ？──否──► 不提示
                                   └─是──► 弹窗（更新日志）──用户同意──►
  计算本地文件 SHA-256 → 只下载变化的对象 → 官方内置 PE 按逐文件索引重建并完整校验
  → update/journal.json 事务替换（主程序最后）→ 重启
```

## 清单格式（`schema_version` = 1）

```json
{
  "schema_version": 1,
  "product": "LetRecovery",
  "version": "v2026.11.13",
  "build_epoch": 1794567890,
  "changelog": [{ "zh-CN": "……", "en-US": "……" }],
  "files": [
    { "path": "LetRecovery.exe", "size": 15820288, "sha256": "…" },
    { "path": "bin/pe/LetRecovery_PE.wim", "size": 218710040, "sha256": "…",
      "wim_index": { "sha256": "…", "size": 412345 } }
  ]
}
```

- `files` 列出除 `config.json` 与清单自身以外的全部包内文件，路径用 `/` 分隔。每个文件的内容对象是 `<对象地址><sha256>`。
- `wim_index` 只出现在 `bin/pe/*.wim`，指向该镜像的逐文件索引对象：
  `{"schema_version":1,"image":1,"boot_index":1,"directories":[…],"files":[[path,size,sha1,sha256,attributes],…]}`。
  镜像内每个文件也是一个对象。典型版本只需下载 `LetRecoveryPE.exe`、`PECMD.INI` 等少数文件，而不是整个 WIM。
- 只允许做加法扩展；客户端忽略未知字段。不兼容的变化必须换新地址，否则旧客户端将无法再升级。

## 客户端行为要点

- 只在官方构建、非测试包、非 WinPE、程序目录存在合法 `bin/release-manifest.json` 时检查；有长任务时本次不提示。
- 下载：系统 TLS/代理优先，失败改直连 Rustls；60 秒无数据视为停滞并用 HTTP Range 续传；每个对象校验大小与 SHA-256。
- `bin/pe` 中的 WIM 被用户自定义（与已安装清单记录的身份不同）时保留不动，并在结果中提示。
- 替换：先写日志，再用同卷不覆盖的 `MoveFileExW(MOVEFILE_WRITE_THROUGH)` 先备份后替换，主程序最后替换；失败立即回滚。断电或崩溃后，下次启动在依赖检查之前前滚或回滚。
- 永不触碰：`config.json`、`log/`、`update/`、`downloads/`、`cli/`、`cache/`、`temp/`。

## 部署清单

1. **服务器**：用仓库外维护的新版 `GetInfo/update.php` 替换 `api.cloud-pe.cn/GetInfo/update.php`。它新增 `letrecovery_app` 模式，原 Cloud-PE One 与 LetRecovery PE 模式的请求格式不变。PHP 进程需要能在 `/www/sites/LetRecovery.cloud-pe.cn/index/update/` 下创建并原子替换 `index.json`（与现有 `v3/`、`v4/` 相同的权限）。
2. **Web**：确认 `https://letrecovery.cloud-pe.cn/update/index.json` 以静态 JSON 提供，最好 `Cache-Control: no-cache`，避免 CDN 长时间缓存旧清单。
3. **R2**：公开地址 `https://r2.cloud-pe.cn/LetRecovery/update/objects/<sha256>` 需要和现有 `LetRecovery/*.7z` 一样可匿名下载，并支持 Range。对象带 `Cache-Control: public, max-age=31536000, immutable`。首次发布约上传 1.7 千个对象（约 0.8 GB），之后每次只上传新内容。
4. **GitHub Secrets**：沿用 `R2_ENDPOINT`、`R2_BUCKET`、`R2_ACCESS_KEY_ID`、`R2_SECRET_ACCESS_KEY`、`UPDATE_API_SECRET`、`LETRECOVERY_UPDATE_API`、`DEPLOY_TOKEN`；`RELEASE_PAT` 需要能推送 `main`（清理已发布的更新日志）。

## 待发布更新日志

仓库根目录 `PENDING_CHANGELOG.json`：

```json
{
  "schema_version": 1,
  "entries": [
    { "en-US": "Fixed …", "zh-CN": "修复……" }
  ]
}
```

每条必须有 `zh-CN` 与 `en-US`，可选 `zh-TW`、`ja-JP`、`ko-KR`、`fr-FR`、`de-DE`。本地校验：

```text
cargo run --locked -p lr-core --example release_update_tool -- validate-changelog PENDING_CHANGELOG.json
```

## 存储增长

对象按内容寻址且从不删除，正在升级的客户端因此始终能下载到一致的数据。如需回收空间，只能删除最近若干个已发布清单（含其 `wim_index` 索引）都不再引用的对象，并保留当前清单引用的全部对象。
