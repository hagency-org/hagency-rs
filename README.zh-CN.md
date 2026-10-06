[English](README.md) | [中文](README.zh-CN.md)

# Hagency

**让本机 Codex agent 为 Palpo Matrix 项目工作，由协调者审批，按 token 预算运行。**

Hagency 是一个 Rust 二进制 `hagency`，在你的机器上作为服务运行，并使用本机安装的 Codex。*运维者*配置资源：模型、推理档位，以及绑定到服务器关联的 token 预算。关联中指定的 Matrix *协调者*在 Rinx 的 Palpo Inbox 审批项目和 agent 申请。额度受理后，Hagency 创建 agent 的 Matrix 身份并启动运行环境。成员在项目房间 @ 提及 agent；agent 所有者可以直接私聊，并在加密的私密房间中审批受保护的操作。

本文描述当前源码版本；较早的预发布二进制可能不包含服务器关联界面。

本仓库包含 [native/](native/) 下的 Rust 服务，以及 [mockup/](mockup/) 下的控制台源码。控制台是一个 Next.js 应用，构建时导出为静态文件，并内嵌在二进制中。

**要运行 Hagency？** 请按[设置 Hagency](#设置-hagency)操作。
**要体验完整的 Rinx 流程？** 请按[快速开始与使用指南](docs/user-guide/README.zh-CN.md)操作。
**要修改服务？** 请先读[代码导读](docs/architecture-walkthrough.zh-CN.md)。

## 目录

| 章节 | |
| --- | --- |
| [功能](#功能) | 能力范围 |
| [架构](#架构) | 一个进程、它的线程与 crate |
| [设置 Hagency](#设置-hagency) | 登录 Codex、启动 Hagency、在控制台中完成设置 |
| [用命令行设置](#用命令行设置) | `hagency setup`、`hagency serve`、安装脚本、协调者安装、运维 API |
| [运维](#运维) | 健康检查、日志、备份、凭据轮换 |
| [配置](#配置) | 状态目录、`fleet-runtime.json` 与 `agent-driver.json` |
| [安全状况](#安全状况) | 哪些是强制的，哪些是假设 |
| [开发](#开发) | 测试与 CI 检查 |
| [文档](#文档) | 指南、设计记录与历史文档 |
| [许可证](#许可证) | Apache 2.0 与 fork 来源 |

## 功能

- **服务器关联。** 在 Hagency 发起关联，资源所有者在 Rinx 确认，再由 Palpo 管理员审批。Hagency 自动接收配置。每个关联有独立的协调者、委托和资源。
- **资源与目录。** 统一从 **我的资源 → 新建资源配置** 选择关联、来源配置、模型、推理档位、预算和可申请项目的用户。一个关联可以配置多个资源。创建资源时就把该预算分配给关联，无需再分配第二层资源池。
- **由项目定义 agent。** 有资格的项目负责人在 Rinx 中从资源申请项目，再为已批准的项目申请 agent。两类申请都由协调者在 Palpo Inbox 审批。项目获批代表可以使用资源；agent 的额度还会单独检查是否符合当前预算。
- **额度与就绪状态。** 审批通过、token 预留和运行就绪是不同状态。测试 agent 前等待 **执行（Execution）· ready**。需要更多 token 时，所有者可在 Rinx 申请。
- **自动配置 Matrix。** Hagency 通过服务器关联的 App Service 创建审批机器人、agent 账号和所有者私聊。指定协调者是负责决策的 Matrix 用户，无需额外运行一个协调者 AI agent。
- **按提及工作。** 在项目房间里，agent 只在有人 @ 提及它时行动。它把周围的讨论作为上下文，并在对话的讨论串里回复。在私聊中，所有者发来的每条消息都会送达它。
- **在其他房间工作。** agent 会加入所有者邀请它进入的房间。其他人的邀请在控制台中等待决定。在已加入的房间里，agent 遵循 [agent 在哪些房间工作](#agent-在哪些房间工作)中的规则（ADR-188）。
- **所有者审批。** 以下操作会以卡片形式发到加密的私密审批室，交给所有者：
  - 沙箱之外的命令和文件修改；
  - 协作类工具；
  - 文件收发。

  只有所有者本人已验证设备的裁决才有效。无人回应的卡片按拒绝处理。
- **一个控制台。** 同一个二进制在回环端口上提供控制台。导航包括设置、我的资源、员工名册、服务器关联、接洽、审批、邀请、用量和告警。项目和 agent 申请在 Rinx 处理。

### agent 在哪些房间工作

| 房间 | 什么会唤醒 agent |
| --- | --- |
| 它与所有者的私聊 | 所有者的每条消息 |
| 项目房间（未加密） | 提及它的真人消息 |
| 已加入的未加密群聊房间 | 提及它的真人消息 |
| 已加入、成员只有所有者和 agent 的房间 | 所有者的每条消息，无论是否加密 |
| 已加入、有其他人的加密房间 | 无。agent 不在那里工作。 |

agent 只为所有者的设备加密。加密房间里的其他人无法读到它的回复。因此在有其他人的加密房间里，agent 保持加入状态，并发一条通知，说明它无法在此工作。只有在别人发言之后，它才会重发通知，且每 15 分钟最多一次。Hagency 每一轮都会重新检查房间。如果其他人离开，agent 就开始在那里工作。

在已加入房间里的工作消耗同一份额度。审批卡片仍然发到所有者的审批室。

## 架构

```text
Palpo homeserver  <── outbound HTTPS ──  hagency start (127.0.0.1:13300)
                                           ├─ Palpo transport: requests, probes, catalog, statuses
                                           ├─ fleet service: provisioning loop, one approval pump per owner
                                           ├─ per agent: a driver thread (Matrix /sync, intake, replies) and an invite poller
                                           ├─ domain + custody SQLite, one writer thread each
                                           ├─ console and operator API
                                           └─ per dispatch: hagency guardian ─> codex app-server ─> hagency mcp
```

- **只有出站连接。** 服务不对外开放任何端口。它轮询 Palpo 的车队 API，并为每个 agent 轮询 Matrix `/sync`，因此可以运行在 NAT 之后。
- **单一二进制。** 同一个可执行文件同时是：
  - 内嵌控制台的守护进程；
  - 用户级服务的注册工具；
  - 掌管每个 runner 进程树的 guardian；
  - 为 Codex 提供任务工具的 MCP 助手；
  - 运维 CLI。
- **车队服务。** 每个已配置的服务器关联有隔离的车队服务，负责创建账号、配置获批的 agent，并为每个所有者运行审批泵。可重试的失败会退避重试；结果不确定的写操作会保留原状态供检查，不会直接重复执行。
- **crate。** Rust 工作区位于 [native/](native/)。主要的 crate 有：
  - `hagency-core`：领域类型；
  - `hagency-store`：持久化规则；
  - `hagency-matrix`：Matrix 副作用；
  - `hagency-palpo`：车队传输层；
  - `hagency-execution` 和 `hagency-runtime`：Codex 运行；
  - `hagency-platform`：进程监管。

  [native/README.md](native/README.md) 列出了全部 crate。

[代码导读](docs/architecture-walkthrough.zh-CN.md)按流程逐一走读代码，并附有图示。

## 设置 Hagency

你需要准备：

| | |
| --- | --- |
| 主机 | macOS，或带 systemd 的 Linux。控制台在运行 Hagency 的这台机器上使用。 |
| 编程代理 | 已安装、且在 `PATH` 上的 Codex CLI |
| Palpo 与 Rinx | 兼容的 Palpo 服务器和 Rinx 客户端，以及该服务器上已有的资源所有者、协调者和管理员账号 |

本文按“中文（English）”标注主要入口。Rinx 的 Palpo 小程序使用当前登录的 Matrix 账号。

终端里完成第 1 到第 3 步，然后在 Hagency 控制台做本机设置，在 Rinx 中处理 Matrix 审批。

### 1. 登录编程代理

在这台机器的终端里，由你自己登录 Codex：

```bash
codex login
```

机器上没有浏览器时，加上 `--device-auth`。

Hagency 从不替你登录。控制台的设置页面只询问 Codex 是否已登录、以何种方式登录（`codex login status`）；`hagency setup` 则检查登录文件是否存在（见[`hagency setup`](#用-hagency-setup-准备状态目录)）。Hagency 从不读取，也从不保存你的凭据。

### 2. 获取 hagency 二进制

**发布构建。** [release-native.yml](.github/workflows/release-native.yml) 为每个平台构建一个内嵌控制台的二进制：macOS arm64（`aarch64-apple-darwin`）和 x86-64（`x86_64-apple-darwin`）、Linux x86-64（`x86_64-unknown-linux-gnu`）和 arm64（`aarch64-unknown-linux-gnu`），另附 `SHA256SUMS`。它只在手动触发时运行，并且不会发布 release：推送标签不会发布任何内容。当前版本 `nv0.1.0-rc.1` 是项目 [GitHub Releases 页面](https://github.com/hagency-org/hagency-rs/releases)上的预发布版本（pre-release）；其中的 `.tar.gz` 资产和 `SHA256SUMS` 由该工作流构建，再手动附加到该版本上。

1. 在 GitHub Releases 页面下载 `SHA256SUMS` 和对应平台的归档 `hagency-nv0.1.0-rc.1-<target>.tar.gz`，其中 `<target>` 为 `aarch64-apple-darwin`、`x86_64-apple-darwin`、`x86_64-unknown-linux-gnu` 或 `aarch64-unknown-linux-gnu`。在 Apple 芯片的 Mac 上，即 `hagency-nv0.1.0-rc.1-aarch64-apple-darwin.tar.gz`。
2. 校验归档。`SHA256SUMS` 列出每个归档的 SHA-256；下面的命令校验你下载的归档，并跳过其他归档：

   ```bash
   shasum -a 256 -c --ignore-missing SHA256SUMS    # Linux：sha256sum -c --ignore-missing SHA256SUMS
   ```

3. 解压归档。其中只有 `hagency` 二进制，已带可执行权限：

   ```bash
   tar -xzf hagency-nv0.1.0-rc.1-aarch64-apple-darwin.tar.gz
   ```

   `./hagency --version` 输出 `0.1.0`：`rc.1` 后缀只出现在标签和资产名称中。

4. 在 macOS 上，这些二进制没有代码签名。请移除下载隔离属性，否则 macOS 会拒绝运行它：

   ```bash
   xattr -d com.apple.quarantine hagency
   ```

从工作流运行的构建产物（artifacts）下载的二进制装在 zip 中，解压后会丢失可执行权限。解压后运行 `chmod +x hagency`。

**从源码构建。** 需要 [rust-toolchain.toml](rust-toolchain.toml) 中固定的 Rust 工具链，以及仅在构建时使用的 Node.js 22。

1. 安装控制台的构建依赖：

   ```bash
   (cd mockup && npm ci)
   ```

2. 把控制台构建到一个新目录：

   ```bash
   node mockup/scripts/build-native-console.mjs --output /abs/path/console
   ```

   脚本拒绝已存在的目录，并以 0700 权限创建新目录。

3. 构建内嵌该控制台的二进制。请使用绝对路径：

   ```bash
   HAGENCY_CONSOLE_DIR=/abs/path/console cargo build --release --locked -p hagency
   ```

   目录中没有控制台的 `manifest.json` 时，构建会拒绝。

不设 `HAGENCY_CONSOLE_DIR` 时，二进制不带控制台。此时 `hagency start` 拒绝运行，除非传入 `--console-assets /abs/path/console`。同一个参数也可以用控制台目录代替内嵌的控制台，供开发控制台时使用。

把二进制放在固定的位置，例如 `~/.local/bin/hagency`，并且该目录要在你的 `PATH` 上。macOS 默认的 `PATH` 不包含 `~/.local/bin`，请在 shell 配置文件中加入它，或用完整路径调用二进制。第 3 步的服务记录的是二进制解析后的路径，因此符号链接不会跟随更新后的二进制。替换二进制后，请重新运行 `hagency service install`：它会重启服务，从而运行新的二进制。

### 3. 启动 Hagency

二选一：

- **作为服务运行（推荐）：**

  ```bash
  hagency service install
  ```

- **在当前终端中运行：**

  ```bash
  hagency start
  ```

  按 Ctrl-C 停止。

`hagency start`：
- 使用默认状态目录：macOS 上是 `~/Library/Application Support/Hagency`，Linux 上是 `${XDG_DATA_HOME:-~/.local/share}/hagency`。要用其他目录，传入 `--state-dir DIR`。
- 目录是新目录或空目录时将其初始化。对于不是 Hagency 状态目录的非空目录，它会拒绝。
- 监听 `127.0.0.1:13300`。`--listen` 只接受本机回环地址。
- 以车队模式运行（`serve --palpo-transport`），并提供内嵌控制台。可以在尚未配置本地 Codex 或 Palpo 关联时启动，之后通过设置和 Rinx 审批流程完成。
- 输出控制台登录链接，并在浏览器中打开它。传入 `--no-open` 时只输出链接。它只把链接输出到交互式终端；否则输出需要运行的 `hagency console-access` 命令。

`hagency service install` 注册一个运行 `hagency start --no-open` 的用户级服务，并启动它：
- **macOS：** 一个 LaunchAgent，即 `~/Library/LaunchAgents/io.hagency.plist`。它在登录时启动，崩溃后自动重启。日志在 `~/Library/Logs/Hagency/hagency.log`。
- **Linux：** 一个 `systemd --user` 单元，即 `${XDG_CONFIG_HOME:-~/.config}/systemd/user/hagency.service`。不需要 `sudo`。用户级服务会在你退出登录时停止。要让它继续运行，运行一次 `loginctl enable-linger $USER`。
- 服务以你的身份运行，因此使用你的 Codex 登录。它记录你当前的 `PATH`，因此找到的 `codex` 和你找到的是同一个。
- 它接受 `--state-dir`、`--listen` 和 `--no-open`，与 `start` 相同。服务响应后，该命令输出登录链接并打开它。
- 它记录二进制解析后的（规范）路径。再次运行它会替换服务，例如在你移动或替换了二进制之后。在两种系统上，再次运行都会重启服务：macOS 上先卸载代理（`launchctl bootout`）再重新加载；Linux 上先运行 `systemctl --user enable --now`，再运行 `restart`，从而运行新的二进制。
- `hagency service uninstall` 停止并删除服务，保留状态目录。

### 4. 打开控制台

在这台机器的浏览器中打开输出的链接。这个链接可以打开整个控制台。打开后，它会换成一个 `HttpOnly` 会话 cookie，重启服务不会让你退出登录。在你生成新链接之前，该链接一直有效，请妥善保管。

生成新链接：

```bash
# macOS
hagency console-access --state-dir "$HOME/Library/Application Support/Hagency"
# Linux
hagency console-access --state-dir "${XDG_DATA_HOME:-$HOME/.local/share}/hagency"
```

如果你给 `start` 改过 `--state-dir` 或 `--listen`，这里也要传入相同的值。`hagency start` 只把链接输出到交互式终端，服务日志里只有这条命令。`console-access` 总是输出链接，因此不要把它的输出重定向到文件。

### 5. 在控制台中完成设置

1. 打开 **设置 → 编程代理（Coding agents）**。Hagency 检查 Codex 和登录状态，然后写入并校验 `fleet-runtime.json`。安装或登录 Codex 后可点 **重新检查（Check again）**。如果 Codex 程序发生变化，按页面提示重启服务。
2. 在 **连接 Palpo** 下点 **新建服务器关联（New server engagement）**。填写 HTTPS Matrix 服务器地址、资源所有者的完整 Matrix ID、协调者的完整 Matrix ID、关联名称和授权期限。使用该服务器上已有的账号。只有管理员提供了单独的管理地址时，才填写 **独立的 Palpo 地址**。
3. 点击 **发起关联（Request connection）**。资源所有者在 Rinx 的 **Palpo → Inbox** 确认请求，再由服务器管理员在自己的 Inbox 审批。Hagency 自动接收已批准的配置；此流程不需要下载或导入 JSON。
4. 资源所有者在 Rinx Inbox 打开已批准的请求，点击一次 **验证连接（Verify connection）**。验证期间按钮禁用；等待 Rinx 和 Hagency 都显示 **连接已验证（Connection verified）**。
5. 打开 **我的资源 → 新建资源配置（New resource configuration）**。选择已验证的关联和已有的本地 **来源配置（Source configuration）**，再选择模型、推理档位、每月 token 预算和可申请项目的 Matrix 用户，最后点 **创建资源（Create resource）**。来源配置提供框架、提供方和账号，并不是 agent、项目或角色。[快速开始](docs/user-guide/README.zh-CN.md#第-5-步创建资源)解释字段含义及首次使用时的来源配置前提。

添加其他资源也使用同一入口。每个资源在该关联下只有一份预算，不需要回到连接页面再次分配。在 **编辑资源配置** 中修改已有资源。预算不能低于已消耗或已预留的 token；资源被使用时，模型变更可能被拒绝。

### 6. 在 Rinx 申请项目和 agent

1. 有资格的项目负责人打开 **Palpo → 资源 → 在此申请项目（Request project here）**，填写名称和用途。可以由 Palpo 创建房间，也可以选择已有的、由自己创建的私密未加密房间。
2. 关联的协调者在 **Palpo → Inbox** 审批项目，随后等待项目配置完成。
3. 在 **Palpo → 项目** 点击 **申请 agent（Request agent）**，选择资源和角色，填写 agent 名称、初始 token 数和每日速率。协调者在自己的 Inbox 审查并批准或拒绝申请的额度。
4. 查看请求的执行状态。**已批准（approved）** 表示决策已记录。私聊邀请出现后先接受，等待 **ready** 再直接发送消息，无需 @。在项目房间中则需要 @ 提及 agent。

[快速开始与使用指南](docs/user-guide/README.zh-CN.md)介绍加密、执行操作审批、追加 token 和配置失败的处理。Hagency 的 **接洽** 页面用于查看运行和额度状态；协调者审批在 Rinx 进行。

## 用命令行设置

以下方式用于自动化、恢复和已有协调者安装。全新安装还需要按[用运维 API 创建资源](#用运维-api-创建资源)创建一次本地来源配置。

### 服务模式

| 模式 | 适用于 | 运行方式 | 配置 |
| --- | --- | --- | --- |
| 车队（默认，推荐） | 使用服务器关联自动创建 agent | `hagency start`，或 `serve --palpo-transport` | 设置或 `hagency setup` 写入的 `fleet-runtime.json`，以及每个关联安装的连接配置 |
| 协调者 | 运行协调者 agent 的已有安装 | `serve --agent-driver --palpo-transport` | `agent-driver.json` 及其 `matrix.*` 和 `approval.*` 文件 |

`--agent-driver` 和 `--development-driver` 互斥。Palpo 交付需要启用 `--palpo-transport`，仅安装配置不会启动传输。已有的协调者模式通过 `agent-driver.json` 配置运行环境；设置页面仍提供服务器关联和资源入口。二进制内嵌了控制台时，`serve` 提供内嵌控制台；`--console-assets` 可以替换它。

### 用 `hagency setup` 准备状态目录

`hagency setup` 做的事与设置页面第一步相同，并提供更多选项：

```bash
hagency setup --state-dir /abs/path/state
```

`hagency setup` 会：
- 在目录是新目录或空目录时，像 `hagency init` 一样初始化它。对于没有 `operator.token` 的非空目录，它会拒绝。
- 查找 Codex 二进制：`--codex PATH`，否则用 `PATH` 上的 `codex`。如果找到的是 npm 启动脚本，setup 会改用 npm 包附带的原生二进制。
- 查找 Codex 登录目录：`--codex-home DIR`，否则是 `$CODEX_HOME`，再否则是 `~/.codex`。该目录必须存在；不存在时，setup 会提示你先运行 `codex login`。传入 `--no-local-codex` 时，setup 不查找这个目录，因此不需要 `~/.codex`：agent 改为登录到 `<state>/runtime-home`，setup 报告的也是这个目录，文件中不写 `local_codex` 块。
- 创建 `<state>-agent-homes`，并以 0600 权限写入 `fleet-runtime.json`，取值为[配置](#配置)中列出的默认值。
- 用 `serve` 所用的同一个加载器校验该文件。校验失败的文件会被改名为 `fleet-runtime.json.rejected`，因此服务绝不会用它启动。
- 已有 `fleet-runtime.json` 时拒绝覆盖，除非传入 `--force`。传入 `--force` 时，旧文件保留为 `fleet-runtime.json.bak-<秒数>`。运行中的服务一直使用启动时读取的配置，因此要重启服务才能使用新文件。

它会输出所选的 Codex 二进制、写入的文件，以及 Codex 是否已登录。它只根据 Codex 登录目录中是否有 `auth.json` 判断是否已登录，不运行 `codex login status`。因此 Codex 只保存在系统钥匙串中的登录在这里显示为未登录，而控制台的设置页面会询问 Codex（`codex login status`）并报告该登录。未登录时，它会输出要运行的命令 `CODEX_HOME=<目录> codex login`。最后列出接下来要运行的命令。如果 `serve` 不使用 `127.0.0.1:13300`，请传入 `--listen`。`--console-assets` 只用于填写输出中的 `serve` 命令。

有 `local_codex` 块时，Codex 运行时 `HOME` 为 `local_codex.home`，`CODEX_HOME` 为 `local_codex.codex_home`。没有该块时，两者都是 `<state>/runtime-home`。托管账户和 `hagency account login` 只用于协调者安装。

### 用 `hagency serve` 在前台运行

```bash
hagency serve --state-dir /abs/path/state --palpo-transport
```

然后用 `hagency console-access --state-dir /abs/path/state` 生成控制台链接，并从[第 5 步](#5-在控制台中完成设置)继续。

### 用 install-native.sh 安装车队

[install/install-native.sh](install/install-native.sh) 用一个二进制和一个控制台目录安装系统服务。它早于 `hagency service install`；对车队安装来说，后者已取代它。

用哪个用户运行它很重要，因为服务以该用户运行，并使用该用户的 Codex 登录：

- **Linux。** 安装脚本把单元写入 `/etc/systemd/system` 并运行 `systemctl`，因此请用 `sudo` 运行它。它把单元的 `User=` 渲染为运行它的用户，因此服务以 `root` 运行。
  - setup 写入的 `local_codex` 绑定只接受属于服务用户（`root`）、且组和其他用户都没有写权限的主目录和 Codex 目录。因此 `--codex-home` 不能指向其他用户的 `~/.codex`。
  - 如果 root 的 `PATH` 上没有 `codex`，请传入 `--codex /abs/path/to/codex`。
  - 在 Linux 上最简单的做法是传入 `--no-local-codex`。安装完成后，用 `sudo env CODEX_HOME=<state>/runtime-home codex login` 把 Codex 登录到服务的运行时主目录。
  - 使用这个安装脚本时，凡是读取状态目录的命令都要以 root 身份（`sudo`）运行，包括 `hagency console-access`。
- **macOS。** 以你自己的身份运行。它为你的用户安装一个 LaunchAgent `io.hagency.native`，使用你的 Codex 登录。

```bash
install/install-native.sh \
  --install-dir /abs/path/bin \
  --state-dir /abs/path/state \
  --console-dir /abs/path/console
```

- **运行之前。** 把二进制复制到 `<install-dir>/hagency`；缺少它时，安装脚本拒绝开始。按[第 2 步](#2-获取-hagency-二进制)构建控制台目录。
- **步骤。** 安装脚本会：
  1. 运行 `hagency init`，它要求状态目录为空，并生成 `operator.token`；
  2. 把 `--config-dir` 中的文件以 0600 权限复制到状态目录；
  3. 运行 `hagency setup`，它查找 Codex 并写入经过校验的 `fleet-runtime.json`。如果 `--config-dir` 已提供 `fleet-runtime.json`，则跳过这一步。setup 失败时，安装停止并显示 setup 的提示；
  4. 在 Linux 上渲染 [deploy/hagency-native.service](deploy/hagency-native.service)，在 macOS 上渲染 [deploy/io.hagency.native.plist](deploy/io.hagency.native.plist)，写入该模式的 `serve` 参数（车队为 `serve --palpo-transport`），并启动它；
  5. 只有 `/ready` 在 60 秒内返回 200 才算成功。
- **Codex 选项。** 安装脚本把以下选项传给 `hagency setup`：
  - `--codex PATH`：Codex 二进制。默认使用 `PATH` 上的 `codex`。
  - `--codex-home DIR`：存放 Codex 登录的目录。默认是 `$CODEX_HOME`，否则是 `~/.codex`。
  - `--no-local-codex`：agent 使用 `<state>/runtime-home`，而不是本机的 Codex 登录。不需要 `~/.codex`。
- **自备 `fleet-runtime.json`。** 把它放进一个目录，并传入 `--config-dir DIR`。安装脚本会复制它，不再运行 setup。该文件必须符合[配置](#配置)中的说明。
- **拒绝情形。** 安装脚本拒绝缺失的 `--console-dir`、非空的状态目录、缺失的二进制、没有 systemd 的 Linux，以及已存在的单元（除非传入 `--overwrite`）。
- **拒绝后重试。** 第 1 步之后的每一种拒绝都会留下已初始化的状态目录：没有 systemd 的 Linux、未传 `--overwrite` 时已存在的单元、`--config-dir` 中有问题的文件，以及 setup 失败。再次运行安装脚本前，请清空状态目录或换一个新目录。setup 失败时，也可以改用 `hagency setup --state-dir …` 完成剩余步骤。

然后用 `hagency console-access --state-dir /abs/path/state` 生成控制台链接，并从[第 5 步](#5-在控制台中完成设置)继续。

### 协调者安装（已有安装）

已有的协调者安装按原配置继续工作。`hagency start` 和设置页面的第一步不适用于它。要把协调者安装设为服务，传入 `--mode coordinator`，以及包含 `agent-driver.json` 的 `--config-dir`：

```bash
install/install-native.sh --mode coordinator \
  --install-dir /abs/path/bin \
  --state-dir /abs/path/state \
  --console-dir /abs/path/console \
  --config-dir /abs/path/config [--overwrite]
```

- **配置。** `agent-driver.json` 是必需的；没有它，安装脚本拒绝开始。`--config-dir` 还可以提供 `fleet-runtime.json`、`development-driver.json`、`palpo-transport.json`，以及私密的 `matrix.*`、`palpo.*` 和 `approval.*` 文件。其他文件名一律拒绝。
- **服务。** 单元运行 `serve --agent-driver --palpo-transport`，不运行 `hagency setup`。其余步骤和拒绝情形与车队相同。

### 用运维 API 创建资源

资源向导目前需要已有的来源配置。在全新状态目录中，**设置** 只配置 Codex 运行环境，不会创建来源资源。运维者需先用 API 创建一次本地 Codex 来源，之后统一从 **新建资源配置** 创建关联资源。这是首次使用的前提，不是另一个需要分配 token 的关联资源池。

替换状态目录；如果服务没有使用默认的 `127.0.0.1:13300`，也要替换地址。以下示例使用 `hagency setup` 写入的 preset 和 seat，并保持来源资源不发布：

```bash
curl -s -X POST http://127.0.0.1:13300/api/native/v1/resources \
  -H "Authorization: Bearer $(cat /abs/path/state/operator.token)" \
  -H 'Content-Type: application/json' \
  -d '{"presetId":"local_codex","seatId":"local_codex_seat","framework":"codex",
       "model":"gpt-5.6-sol","provider":"openai","reasoning":"medium",
       "ceiling":{"tokens":20000000,"period":"monthly"},"published":false}'
```

使用 Linux 的 install-native.sh 安装时，只有 root 能读取 `operator.token`，因此需要 root shell（`sudo -s`）。只在 `curl` 前加 `sudo` 不够，`$(cat …)` 仍以当前用户身份运行。无需先登记 seat，响应是资源的目录记录。

- **只发布有资格的组合。** 模型和推理档位必须符合 [role-capacity.json](native/hagency-core/role-capacity.json) 中至少一个角色的资格，否则即使已保存，也不会发布。资源向导只提供有资格的组合。
- **匹配本机登录。** 使用 `local_codex` 时，`seatId` 必须等于 `local_codex.seat`，`framework` 必须是 `codex`，`provider` 为 `openai` 或省略。setup 写入 `local_codex` 和 `local_codex_seat`，与示例相同。该 API 不检查这些值是否与运行配置匹配；配置不匹配时，即使申请获批，也无法创建 agent。

## 运维

下面的命令针对 `hagency service install` 注册的用户级服务。install-native.sh 安装的是另一个服务：

- **Linux：** 一个系统级单元 `hagency-native`，位于 `/etc/systemd/system`。请使用系统范围，加 `sudo`、不加 `--user`：`sudo systemctl status|restart|stop|start hagency-native`、`sudo journalctl -u hagency-native`。下面的 `hagency` 命令也要以 root 身份运行，因为状态目录属于服务用户。
- **macOS：** 一个标签为 `io.hagency.native` 的 LaunchAgent（`~/Library/LaunchAgents/io.hagency.native.plist`）。在 `launchctl` 命令中使用这个标签，例如 `launchctl kickstart -k gui/$(id -u)/io.hagency.native`。

| 任务 | 命令 |
| --- | --- |
| 检查存活 | `curl -s 127.0.0.1:13300/health` |
| 检查就绪 | `curl -s 127.0.0.1:13300/ready`。返回 503 时会列出每个未就绪的组件。 |
| 服务状态（Linux） | `systemctl --user status hagency` · `journalctl --user -u hagency` |
| 日志（macOS） | `~/Library/Logs/Hagency/hagency.log`。使用 install-native.sh 时：`<install-dir>/logs/hagency-native.stdout.log` 和 `…stderr.log`。 |
| 设置日志级别 | `RUST_LOG`（默认 `info`） |
| 重启 | `launchctl kickstart -k gui/$(id -u)/io.hagency`（macOS）· `systemctl --user restart hagency`（Linux）。崩溃的进程也会由服务重新拉起。 |
| 停止 | `launchctl bootout gui/$(id -u)/io.hagency`（macOS）· `systemctl --user stop hagency`（Linux）。停止只是暂时的：持续到你下次登录，或你再次启动服务为止。 |
| 再次启动 | `launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/io.hagency.plist`（macOS）· `systemctl --user start hagency`（Linux）。也可以重新运行 `hagency service install`。 |
| 停止并删除 | `hagency service uninstall`，保留状态目录。 |
| 新的控制台链接 | `hagency console-access --state-dir <state>` |
| 查看 | `hagency engagements`、`hagency resources`、`hagency alerts`（带 `--state-dir`；加 `--json` 输出原始内容） |
| 在线备份 | `hagency backup --state-dir <state> --out <new dir>` |
| 恢复 | `hagency restore --state-dir <empty dir> --from <backup>` |
| 轮换运维令牌 | `hagency rotate --state-dir <state> operator-token`。重启服务后新令牌才生效。 |

**车队服务进度。** 车队服务每次切换阶段都会记录日志 `fleet service stage`。阶段如下：

1. `awaiting_runtime_config`：缺少 `fleet-runtime.json`。请在控制台完成 **设置（Setup）→ 编程代理（Coding agents）**，或运行 `hagency setup`。
2. `awaiting_reception`：Palpo 的 **Verify connection** 还没有绑定接待房间。
3. `identities`：服务正在创建车队的账号和密钥。
4. `running`：创建循环和审批泵正在运行。

两个 `awaiting_*` 阶段每 5 秒重新检查一次。`identities` 阶段失败，以及服务拒绝配置（显示为 `refused_config`）时，按 1 秒到 60 秒的退避重试。

**Codex 更新之后。** `fleet-runtime.json` 固定了 Codex 二进制的路径和 SHA-256。车队服务只在服务启动时读取并检查一次该文件；运行中的服务在重启之前一直使用已读取的配置。Codex 更新后：

1. 在控制台中打开 **设置（Setup）**。它会提示编程代理已变化，重写配置，并把旧文件保留为 `fleet-runtime.json.bak-<秒数>`。
2. 按页面提示重启服务：`launchctl kickstart -k gui/$(id -u)/io.hagency`（macOS）、`systemctl --user restart hagency`（Linux），或停止 `hagency start` 后重新运行。运行中的服务不会自行读取重写后的文件。

控制台按 `hagency setup` 的默认值重写文件：`local_codex` 取自 `$CODEX_HOME` 或 `~/.codex`。如果你当初运行 `hagency setup` 时用了 `--codex-home` 或 `--no-local-codex`，请改用相同的选项再次运行 `hagency setup --state-dir <state> --force`，然后重启。不用控制台时，也用这条命令重写文件。如果重启后的服务仍然拒绝该文件，它会显示 `refused_config`，并按 1 秒到 60 秒的退避重试，因此改正后的文件无需再次重启即可被读取。

**所有者密钥。** Hagency 第一次需要某个所有者的交叉签名主密钥时，会从 homeserver 读取该密钥，并固定（pin）在存储中。没有开启交叉签名的所有者还没有密钥，因此该所有者的 agent 会等待。已固定的密钥不会被 homeserver 之后报告的密钥替换。控制台和 CLI 目前还不提供重新固定的操作。因此，重置了交叉签名的所有者在固定的密钥被更改之前无法得到服务。即便重新固定，也修复不了已经注册的 agent：它们冻结的密钥列表仍保留旧密钥，因此在每个 agent 重新创建之前，它们发给该所有者的消息都会失败；该所有者也会得到一个新的审批设备（ADR-187 修订；见[已知缺口](docs/architecture-walkthrough.zh-CN.md#15-已实现尚未实现与已知缺口)）。

**agent 等待所有者时重启。** 新 agent 等待其所有者加入私聊期间重启服务，会使该 agent 停滞。见使用指南的[已知限制](docs/user-guide/README.zh-CN.md#已知限制)。

**关闭。** 收到 SIGTERM 时，服务先停止 runner、Palpo 传输和车队服务，然后关闭 Matrix 会话和数据库，最后停止 HTTP 服务。安装脚本的 systemd 单元给它 20 秒。

## 配置

服务不读取任何环境文件。它的配置就是状态目录中的文件：

| 文件 | 用途 |
| --- | --- |
| `operator.token` | 运维 bearer 密钥，由 `hagency init` 生成 |
| `fleet-runtime.json` | 导入车队的 Codex 运行时设置，见下文。由设置页面或 `hagency setup` 写入。 |
| `<state>-agent-homes/`（同级目录） | agent 的主目录，与凭据和 SDK 状态隔离（即 `hagency setup` 写入的 `home.root`） |
| `palpo-transport.json`、`palpo.machine_token`、`palpo-appservice.json` | 由 Palpo 导入写入 |
| `representative.identity.json`、`matrix.representative_token`、`matrix.appservice_token`、`matrix.provisioning_key` | 车队代表和创建 agent 所用的凭据。车队服务只创建一次。 |
| `approval-<owner>.*`、`approval-sdk-<owner>/` | 每个所有者一个审批机器人设备：它的记录、令牌、身份和密钥文件，以及 SDK 存储。`<owner>` 是由所有者 Matrix ID 派生的标识。在车队服务第一次为该所有者准备已批准的 agent 时创建（前提是该所有者已有交叉签名密钥）。 |
| `agent-driver.json`、`matrix.*`、`approval.*` | 仅用于协调者安装：runner、Matrix 和审批机器人设置 |
| `agent-matrix-provision_<engagement>/` | 每个 agent 一个：它的加密创建记录和房间记录，以及它的 Matrix SDK 存储 |
| `runtime-home/` | `fleet-runtime.json` 中没有 `local_codex` 块时，Codex 的 `HOME` 和 `CODEX_HOME`；存放 Codex 登录 |
| `fleet-workspace/` | 车队执行主机的私密占位工作区；不会有派发在其中运行 |
| `factory-task-contexts/` | 预热任务桥使用的每次派发的任务上下文文件 |
| `console-logins.json` | 控制台访问链接和登录的 SHA-256 哈希，因此重启不会让你退出登录 |
| `domain.sqlite3`、`custody.sqlite3` | 持久状态（WAL，`synchronous=FULL`）。每个状态目录只能有一个进程使用。 |

配置文件必须仅所有者可读写（权限 0600）。`fleet-runtime.json` 和 `agent-driver.json` 拒绝未知字段。

设置页面和 `hagency setup` 用同一套代码写入并校验 `fleet-runtime.json`；下表是该文件的格式，供阅读它，或通过安装脚本的 `--config-dir` 自备文件时参考。setup 写入以下取值：

- `executable` 和 `executable_sha256`：它找到的 Codex 二进制及其哈希；
- `file_limit` 4194304（4 MiB）、`operation_ms` 300000、`response_ms` 2000、`approval_owner_wait_ms` 180000、`idle_ms` 1200000；
- `send_file` 和 `receive_file` 为 `true`；
- `home`：`root` 为 `<state>-agent-homes`，`task_client` 为正在运行的 `hagency` 二进制，`projects` 为 `[]`；
- `local_codex`（除非传入 `--no-local-codex`）：preset 为 `local_codex`，席位为 `local_codex_seat`，`home` 为你的 `HOME`，`codex_home` 为 Codex 登录目录（`$CODEX_HOME`，或 `~/.codex`；设置页面总是使用这个默认值，`hagency setup --codex-home` 可以另选目录）。

要修改某个值，可以编辑该文件（保持 0600 权限）并重启服务，或换用其他选项运行 `hagency setup --state-dir <state> --force`。

`fleet-runtime.json` 的字段：

| 字段 | 必填 | 含义 |
| --- | --- | --- |
| `profile` | 是 | 必须是 `palpo_fleet_runtime_v1` |
| `executable` | 是 | Codex 二进制：不含符号链接的绝对路径 |
| `executable_sha256` | 是 | 该二进制的 SHA-256，64 个小写十六进制字符 |
| `local_codex` | 否 | 绑定运维者自己的 Codex 登录（子字段见下）。不设置时，Codex 使用 `<state>/runtime-home`。 |
| `file_limit` | 是 | 文件工具的文件大小上限，单位字节：1 到 4,194,304。即使两个文件工具都关闭，也会检查。 |
| `operation_ms` | 是 | 每次派发的操作预算，100 到 1,200,000 毫秒。不得小于 `approval_owner_wait_ms` + 5000。 |
| `response_ms` | 是 | 每次派发的响应预算，10 到 2000 毫秒 |
| `idle_ms` | 是 | 预热的运行时可以空闲多久，范围 100 到 1,200,000 毫秒 |
| `approval_owner_wait_ms` | 否 | 卡片等待所有者多久后按拒绝处理。缺少该字段时为 1000 毫秒；`hagency setup` 写入 180000。加上 5000 毫秒的回复预留后不得超过 600,000 毫秒。 |
| `send_file`、`receive_file` | 否 | 启用文件工具。默认 `false`。 |
| `coordination_tools` | 否 | 启用委派和对等工具。每次调用都需所有者批准。默认 `false`。 |
| `matrix_request_interval_ms`、`matrix_sdk_timeout_ms` | 否 | Matrix 请求节流，以及 SDK 预算（10 到 60,000 毫秒） |
| `home` | 是 | agent 主目录，含三个必填子字段：`root`，一个已存在、仅所有者可访问的目录；`task_client`，`hagency` 二进制的绝对路径；`projects`，最多 16 项，每项含 `project_id`、`source` 和 `mode`（`copy` 或 `symlink`）。`projects` 可以是 `[]`：没有对应条目的项目会得到一个不复制源码的主目录。 |

`local_codex` 的子字段，出现该块时全部必填：

| 字段 | 含义 |
| --- | --- |
| `profile` | 必须是 `provider_owned_codex_v1` |
| `preset` | 一个资源 preset id，记录在这个登录所服务的每个认领上 |
| `seat` | 这个登录服务的席位 id。`seatId` 与之相同的 Codex 资源在这个登录下运行。 |
| `home` | Codex 的 `HOME` 目录 |
| `codex_home` | Codex 的 `CODEX_HOME` 目录，存放 Codex 登录 |

`home` 和 `codex_home` 必须是不含符号链接的绝对路径，属于服务用户，且组和其他用户不可写。

`hagency setup` 写入的内容如下，其中每个 `/srv/hagency/...` 路径代表它找到的实际路径：

```json
{
  "profile": "palpo_fleet_runtime_v1",
  "executable": "/srv/hagency/bin/codex",
  "executable_sha256": "<64 个小写十六进制字符：codex 二进制的 SHA-256>",
  "local_codex": {
    "profile": "provider_owned_codex_v1",
    "preset": "local_codex",
    "seat": "local_codex_seat",
    "home": "/srv/hagency/home",
    "codex_home": "/srv/hagency/home/.codex"
  },
  "send_file": true,
  "receive_file": true,
  "file_limit": 4194304,
  "operation_ms": 300000,
  "response_ms": 2000,
  "approval_owner_wait_ms": 180000,
  "idle_ms": 1200000,
  "home": {
    "root": "/srv/hagency/state-agent-homes",
    "task_client": "/srv/hagency/bin/hagency",
    "projects": []
  }
}
```

这里不出现 Matrix 身份。它们来自导入的车队。

`agent-driver.json` 用于配置协调者安装。它与上表共用 Codex、预算和文件相关字段，另外增加 `workspaces`、`matrix` 和 `approval` 身份块，以及 `factory_service`。

权威定义是 [native/hagency/src/bootstrap/config.rs](native/hagency/src/bootstrap/config.rs) 中的 `FleetRuntimeConfig` 和 `Config`。

## 安全状况

Hagency **强制执行**的：

- **仅限回环地址。** 服务拒绝任何非回环的监听地址。控制台在登录和每次写操作时都检查 `Host` 头，并检查 `Origin` 是否为 `http://<监听地址>`（[native/hagency/src/console.rs](native/hagency/src/console.rs)），因此不能放在反向代理之后。请在运行 Hagency 的机器上打开控制台。从其他机器访问不是受支持的部署方式。
- **控制台请求。** 控制台要求完全匹配的 `Host` 头，并拒绝转发类请求头。写操作需要同源的 `Origin`。运维 API 同样要求完全匹配的 `Host`，并拒绝 `Forwarded`、`X-Forwarded-For`、`Origin` 和 `Sec-Fetch-Site`，然后以常数时间比较 bearer 令牌的 SHA-256。
- **所有者审批。** 审批只来自所有者已验证的设备。审批在一个加密房间中进行，房间成员只有所有者和审批机器人。出现第三个成员或失去加密时，该房间会被停用。投递失败或等待超时都按拒绝处理。
- **每个所有者一个审批设备。** 每个所有者的审批机器人设备只信任该所有者。不同所有者的审批相互隔离。
- **受隔离的 runner。** 每次派发都有一个能力凭证和一个隔离编号（fence）。过期 runner 的调用会被拒绝。只有证明其进程树已经消失后，回复才会发布。
- **Codex 沙箱。** Codex 以 `workspace-write` 和 `on-request` 审批运行，没有网络，只有自己的工作区可写。Hagency 会检查 Codex 回显的设置。
- **凭据只写不读。** 没有任何路由会返回已存储的令牌。Palpo 导入的响应只包含公开信息。
- **不接触编程代理的凭据。** 设置页面只运行它在 `PATH` 上找到的 Codex 二进制，只带 `--version` 和 `login status` 参数，并按路径和 SHA-256 记录它。Hagency 从不执行登录，也从不读取或保存代理的凭据。每次设置写入都需要运维者的控制台会话。
- **登录链接不进日志。** `hagency start` 只把控制台链接输出到交互式终端。以服务方式启动时，日志中只写 `console-access` 命令。
- **加固的单元（仅限安装脚本）。** install-native.sh 的 systemd 单元设置了 `NoNewPrivileges`、`ProtectSystem=full`、空的 capability 集合和系统调用过滤。`hagency service install` 注册的用户级服务没有这些设置，它以你的用户权限运行。

Hagency **假设**的：

- **主机内共享信任。** 回环信任覆盖整台机器，所以任何本地进程都能访问该端口。请保持 `operator.token` 和状态目录私密。
- **所有者密钥首次使用即信任。** 如果 homeserver 在 Hagency 第一次读取所有者密钥时作假，Hagency 会信任错误的密钥。之后 homeserver 的回答不会替换已固定的密钥。
- **群聊房间对成员开放。** 任何已加入的成员只要 @ 提及 agent，就能给它派活。房间成员资格、额度和所有者审批是控制手段。
- **加密的多人房间无法使用。** agent 不能在有所有者以外其他人的加密房间里工作。
- **不使用联邦。** 所有成员都必须在车队自己的服务器上。
- **沙箱资格验证尚未完成。** Hagency 会请求并检查沙箱，但各操作系统上沙箱实际效果的资格验证仍未完成。

## 开发

运行与 CI 相同的检查：

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
node native/scripts/check-rust-spec-bindings.mjs
node --test native/scripts/check-production-callers.test.mjs
node native/scripts/check-production-callers.mjs
cargo test --workspace --all-targets --locked --no-fail-fast -- \
  --skip native_codex_real_app_server \
  --skip native_two_agent_qualification_records_its_evidence
```

- `check-rust-spec-bindings.mjs` 检查每个规格选择器都对应一个真实的测试。
- `check-production-callers.mjs` 检查每一行 `Production caller:` 都能在生产调用图中找到。`check-production-callers.test.mjs` 测试这个检查器本身。
- 跳过的两个测试需要真实的、已通过资格验证的主机（ADR-140、ADR-144）。它们仍留在规格中，绑定检查照常覆盖它们。

[specs/](specs/) 中的任务契约把行为绑定到测试。[knowledge/decisions/](knowledge/decisions/) 记录了背后的决策。

以下两个工作流运行这些检查：
- [ci.yml](.github/workflows/ci.yml) 在 Ubuntu 上运行上述检查，覆盖拉取请求、推送到 `main` 和手动触发。只改动 `docs/`、`knowledge/` 或两份 README 的变更不会触发它。
- [rust.yml](.github/workflows/rust.yml) 增加 macOS 和 Windows、控制台浏览器测试和发布构建。它每晚运行，也在手动触发和推送 `nv*` 标签时运行；带 `full-ci` 标签或改动其所列路径的拉取请求也会触发它。

导读中的[如何做改动](docs/architecture-walkthrough.zh-CN.md#16-如何做改动)一节说明新规则、迁移、agent 工具和控制台路由应放在哪里。

## 文档

| 文档 | 内容 |
| --- | --- |
| [docs/user-guide/README.zh-CN.md](docs/user-guide/README.zh-CN.md) | 连接 Palpo、房间、谁能和 agent 对话、token |
| [docs/architecture-walkthrough.zh-CN.md](docs/architecture-walkthrough.zh-CN.md) | 按流程走读代码 |
| [native/README.md](native/README.md) | Rust 工作区：crate、命令、测试（英文） |
| [knowledge/decisions/](knowledge/decisions/) | 架构决策记录 |
| [specs/](specs/) | 绑定到测试的任务契约 |
| [docs/guides/](docs/guides/) | 中文指南：Matrix 对话、文件、agent 状态、Palpo 出站传输 |
| [docs/history/](docs/history/) | 已移除的 TypeScript 产品的设计文档，仅作历史保留（大部分为英文） |
| [docs/LICENSING.md](docs/LICENSING.md) | fork 来源与 Apache 2.0 署名义务 |

## 许可证

**Apache License 2.0**：见 [`LICENSE`](LICENSE) 和 [`NOTICE`](NOTICE)。

Hagency 是 [agent-chat](https://github.com/shisuiki/agent-chat) 的一个 fork，上游于 2026-07-29 采用 Apache 2.0。`NOTICE` 记载了上游作者。再分发时请保留 `NOTICE` 和 `LICENSE`，并标明你修改过的文件。见 [docs/LICENSING.md](docs/LICENSING.md)。
