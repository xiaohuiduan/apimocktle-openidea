---
name: build-and-release
description: 构建、打包、导入表复核、安装与授权的实际路径和陷阱
paths:
  - Cargo.toml
  - script/**
  - dist/**
  - README.md
---

# 构建、打包与安装

一条命令重建组件，一条命令打出可安装包。所有路径与版本约定都在这里，别照记忆猜。

---

## 1. Scope / Trigger

- 改 `Cargo.toml`（依赖、profile、target 相关）
- 改 `plugin.toml`（版本、引擎区间、wasm 路径）
- 改 `script/package.ps1`
- 升级 `openidea-plugin` SDK tag
- 任何"发布/安装"相关操作

---

## 2. Signatures

```powershell
# 构建 + 打包（推荐，一步到位）
pwsh script/package.ps1

# 只构建
cargo build --release --target wasm32-wasip2

# 组件级校验与本地链接（需 rustc ≥ 1.96，本机 1.94 不可用）
openidea-plugin check
openidea-plugin link
```

`script/package.ps1` 的实际动作：

```powershell
$version = (Select-String -Path plugin.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
cargo build --release --target wasm32-wasip2
Copy-Item target/wasm32-wasip2/release/openidea_apimocktle.wasm dist/plugin.wasm -Force
Set-Content dist/plugin.wasm.sources "built <HH:mm:ss> from <git rev-parse --short HEAD>"
Compress-Archive <stage> dist/apimocktle-openidea-$version.zip
```

---

## 3. Contracts

### 产物路径（name 里的 `-` 变 `_`，这是 Cargo 的规则）

```
src (crate openidea-apimocktle)
  → target/wasm32-wasip2/release/openidea_apimocktle.wasm
  → dist/plugin.wasm                                  # plugin.toml 的 wasm 字段指向这里
  → dist/apimocktle-openidea-<version>.zip            # 可安装包
```

crate 的 `[lib] crate-type = ["cdylib", "rlib"]` 是必需的：`cdylib` 出组件，
`rlib` 让 `cargo test` 与 `examples/` 能以库方式链接同一份代码。

### zip 内容（宿主按包布局读取）

```
plugin.toml            # 根
settings.schema.json
README.md
dist/plugin.wasm       # 与 plugin.toml 的 wasm = "dist/plugin.wasm" 一致
icons/*.svg            # 清单里 icon = "icons/apimocktle.svg"
```

少任何一个都会让安装后启动失败或图标空白。`script/package.ps1` 用 `$env:TEMP/apimocktle-pack-$version`
做暂存目录，结束即删。

### 版本与引擎区间（三处一致）

| 位置 | 当前值 | 谁读 |
| - | - | - |
| `plugin.toml` `version` | `0.2.0` | 宿主插件列表；`package.ps1` 取它命名 zip |
| `plugin.toml` `engines.openidea` | `^1.7` | 宿主加载前的兼容检查 |
| `Cargo.toml` `openidea-plugin` `tag` | `v0.1.44` | 构建时拉取的 WIT/绑定 |

**`tag` 与 `engines.openidea` 成对升级**。升 IDE 时同时改，否则组件里的接口与宿主对不上，加载直接失败。

### 导入表实证（改完 SDK 后照此复核）

`dist/plugin.wasm` 里的接口模块全部带 `@1.7.0` 后缀：

```
openidea:plugin/{commands,editor,endpoints-provider,event-handler,events,invocation,
                 lifecycle,log,net,settings,storage,types,ui,view-provider,
                 view-types,views,workspace}@1.7.0
```

复核办法（不需要任何额外工具）：

```powershell
$t = [Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes('dist/plugin.wasm'))
[regex]::Matches($t, 'openidea:[a-z-]+/[a-z-]+@[0-9.]+') | % Value | Sort-Object -Unique
```

出现别的主版本号就是 tag 与引擎区间不一致。

### 安装两条路

**从 zip**（推荐，本机可用）：设置 › 插件 › 齿轮 › 从插件包文件安装…
装好后在插件详情里用 **从磁盘重新加载** 刷新，不必重启 IDE。

**开发期链接**（需 rustc ≥ 1.96）：

```sh
cargo install --locked --git https://github.com/l0ng-ai/openidea ide-host --tag v0.1.44 --bin openidea-plugin
openidea-plugin check
openidea-plugin link
```

宿主只在**启动时**和 `openidea-plugin link` 时读链接清单，所以链接后必须重启 OpenIDEA。

### 本机状态（Windows，OpenIDEA 0.1.44）

```
%APPDATA%\ai.l0ng.openidea\plugins\links.json    # 指向本仓库目录
%APPDATA%\ai.l0ng.openidea\grants.json           # 已记录授权，改动前备份 grants.json.bak
```

已记录的能力：`project.read`、`storage`、`net:localhost`、`net:127.0.0.1`、`clipboard`。

> **Warning**：手写 `grants.json` 必须是**无 BOM 的 UTF-8**。
> 带 BOM 时宿主反序列化失败、账本被静默重置为空，插件停在"需要授权"且日志不报错。
> 这个坑真实踩过一次，排查耗时很长——写文件前先确认编码。

### 环境事实

| 项 | 值 | 影响 |
| - | - | - |
| rustc / cargo | 1.94.0 | 能编 edition 2024 与 wasm32-wasip2 |
| 已装 target | `wasm32-wasip2`、`x86_64-pc-windows-msvc` | 前者出组件，后者跑测试与 examples |
| `ide-host` 要求 | rustc ≥ 1.96（wasmtime 49） | `openidea-plugin check/link` 在本机**跑不了**，只能走 zip |
| 仓库 git | 无 `.git` | `package.ps1` 写的 commit 为空串，属预期 |

---

## 4. Validation & Error Matrix

| 条件 | 现象 | 处理 |
| - | - | - |
| 未装 `wasm32-wasip2` | `cargo build` 报 target 缺失 | `rustup target add wasm32-wasip2` |
| `cargo build` 失败 | `package.ps1` 抛 `cargo build failed` | 先单独 `cargo test` 定位 |
| `dist/plugin.wasm` 与源码不同步 | 装了旧行为 | 用 `package.ps1` 重建，别只 `cargo build`（后者不刷新 dist） |
| zip 内缺 `settings.schema.json` | 设置页空白 | 检查 `$stage` 拷贝列表 |
| `tokens`/能力未授权 | 命令报 `Denied` 或功能沉默 | 插件详情里逐项授权 |
| BOM 的 `grants.json` | 插件停在"需要授权"，日志无错 | 重写为无 BOM UTF-8 |

---

## 5. Good / Base / Bad Cases

**Good：一次完整的发布**

改 `plugin.toml` 的 `version` 与 `change-notes` → `cargo test` → `pwsh script/package.ps1` →
确认 zip 大小与 `dist/plugin.wasm.sources` 时间戳 → 用 zip 安装 → 重启 IDE → 打开项目看仪表盘出接口。

**Base：只改 HTML/CSS**

`src/page.html` 通过 `include_str!` 编进组件，所以改页面也必须重新 `cargo build` + 打包。
它不是外部资源，无法热更新。

**Bad：只跑 `cargo build` 就宣称"已更新插件"**

`dist/plugin.wasm` 没被刷新，宿主加载的还是旧组件。`package.ps1` 里那行 `Copy-Item` 就是为此存在。

---

## 6. Tests Required

| 检查 | 命令 | 通过标准 |
| - | - | - |
| 单元/集成测试 | `cargo test` | `21 passed; 0 failed` |
| 组件可构建 | `cargo build --release --target wasm32-wasip2` | 退出码 0，`target/.../openidea_apimocktle.wasm` 存在 |
| 打包 | `pwsh script/package.ps1` | 打印 `packed dist/apimocktle-openidea-<v>.zip (… KiB)` |
| 接口版本对齐 | 上面那段正则复核 | 只有 `@1.7.0` |
| 真实项目冒烟 | `cargo run --example scan_dir -- <项目>` | 输出接口数与框架分布，无 panic |

`openidea-plugin check` 在本机不可执行，所以**不要**把它写进验收清单；能写的是正则复核导入表。

---

## 7. Wrong vs Correct

### 改完代码只构建不打包

#### Wrong

```sh
cargo build --release --target wasm32-wasip2
# 然后去 IDE 里点"从磁盘重新加载"，以为生效
```

#### Correct

```sh
pwsh script/package.ps1     # 构建 + 刷新 dist/plugin.wasm + 出 zip
```

### 升级 IDE 时只改一处版本

#### Wrong

```toml
# plugin.toml
engines.openidea = "^1.8"
```

```toml
# Cargo.toml 仍是
openidea-plugin = { git = "...", tag = "v0.1.44" }
```

组件按 1.7 的 WIT 编译，宿主按 1.8 校验 → 加载失败，且提示往往只说"接口不匹配"。

#### Correct

```toml
# 两处一起改成成对的版本
openidea-plugin = { git = "https://github.com/l0ng-ai/openidea", tag = "v0.1.45" }   # 举例
engines.openidea = "^1.8"                                                         # 举例
```

改完跑一次导入表正则复核，确认全是对应版本号。
