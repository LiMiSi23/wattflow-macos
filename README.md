# WattFlow macOS

> **Repository name:** WattFlow macOS
>
> **Installed application name:** Powerflow

WattFlow macOS is a Mac-only derivative of [lzt1008/powerflow](https://github.com/lzt1008/powerflow). It provides a native view of live power use, directional power flow, and locally stored history on Apple-silicon Macs. The application itself intentionally keeps the name **Powerflow**.

The current source version is **0.3.0**. Pre-release downloads are available from [GitHub Releases](https://github.com/LiMiSi23/wattflow-macos/releases).

## Features

- Live system input, system load, battery power, screen power, and SoC power readings.
- Directional flow visualization from input to system load to battery, with the system-input series shown in yellow.
- Screen and SoC visibility switches. Unsupported readings appear as `*W` / **Unavailable** and are omitted from the chart.
- A live chart containing up to the latest 100 samples. Turning the chart off pauses chart sampling and saving.
- Manual chart save without clearing the active samples, plus a separate clear-and-restart action.
- Optional automatic chart saving, off by default. Closing with the red window button or choosing Quit from the menu-bar icon saves charts with at least 30 samples; `Cmd+Q` does not auto-save.
- Local history browsing, JSON export, per-record deletion, and **Delete All History**. Delete All also clears the active unsaved chart and performs SQLite checkpoint/vacuum cleanup.
- Compact history records that omit per-sample battery percentages and raw collector payloads. Legacy chart history is compacted during migration.
- English and Simplified Chinese interfaces.

## Compatibility and limitations

- This distribution targets **Apple silicon (`arm64`) Macs only**.
- iOS-device monitoring from upstream Powerflow is disabled in this Mac-only build.
- The bundle identifier remains `Powerflow`, so installing this build replaces an existing upstream Powerflow installation.
- Power collection relies on macOS private APIs, SMC, and IOKit behavior that may change in future macOS releases.
- Screen-power data is not exposed on every Mac/macOS combination; Powerflow reports it as unavailable instead of treating it as `0.0W`.

## Install the pre-release

1. Download the Apple-silicon `.dmg` from [Releases](https://github.com/LiMiSi23/wattflow-macos/releases).
2. Open the disk image and drag **Powerflow** into **Applications**.
3. Open Powerflow from Applications.

The current pre-release is ad-hoc signed and **not Apple-notarized**. If Gatekeeper blocks the first launch, open **System Settings → Privacy & Security**, find the Powerflow notice, choose **Open Anyway**, and confirm **Open**. Only do this for a file downloaded from this repository's Releases page; compare its SHA-256 value with the release notes when one is provided.

## Build from source

Requirements:

- An Apple-silicon Mac with Xcode Command Line Tools
- Node.js 22
- pnpm `10.0.0-rc.0` (also pinned in `package.json`)
- Stable Rust with the `aarch64-apple-darwin` target

```bash
corepack enable
corepack prepare pnpm@10.0.0-rc.0 --activate
rustup target add aarch64-apple-darwin
pnpm install --frozen-lockfile
pnpm tauri build --target aarch64-apple-darwin --bundles app
```

Run the same core checks used by CI:

```bash
pnpm build
cargo test --workspace --locked
cargo check --workspace --all-targets --locked --target aarch64-apple-darwin
```

## Privacy and local data

Power measurements and history stay in the application's local SQLite database; this build does not implement telemetry upload. Opening a GitHub link or exporting a history record only happens after a user action. **Delete All History** removes all app-managed history rows, clears the current chart, checkpoints/truncates SQLite sidecar state, and vacuums the database. External backups or filesystem snapshots remain outside the application's control.

## License and attribution

This project is distributed under the [MIT License](LICENSE). It preserves the upstream Powerflow copyright and records the modified-work copyright. See [NOTICE.md](NOTICE.md) for the upstream repository and base revision.

Issues and source contributions are welcome in [LiMiSi23/wattflow-macos](https://github.com/LiMiSi23/wattflow-macos/issues).

---

## 中文

> **仓库名称：** WattFlow macOS
>
> **安装后的应用名称：** Powerflow

WattFlow macOS 是 [lzt1008/powerflow](https://github.com/lzt1008/powerflow) 的 Mac 专用修改版，可在 Apple 芯片 Mac 上原生显示实时功耗、功率流向和本地历史记录。应用本身仍保留 **Powerflow** 这个名称。

当前源码版本为 **0.3.0**。预发布安装包可从 [GitHub Releases](https://github.com/LiMiSi23/wattflow-macos/releases) 下载。

### 主要功能

- 实时显示系统总输入、系统功耗、电池功率、屏幕功耗和 SoC 功耗。
- 用箭头显示“输入 → 系统功耗 → 电池”的方向，并将系统总输入曲线显示为黄色。
- 可在设置中分别显示或隐藏屏幕与 SoC 功耗。不支持的读数显示为 `*W` / **不可用**，同时不在图表中画线。
- 实时图表最多保留最近 100 个采样点；关闭“显示图表”后暂停图表采样和保存。
- 可手动保存当前图表且不清空现有数据，也可单独清空图表并重新开始记录。
- “自动保存图表”默认关闭。点击窗口红色关闭按钮或从菜单栏图标选择退出时，至少有 30 个采样点的图表会被保存；`Cmd+Q` 不会自动保存。
- 可查看本地历史、导出 JSON、删除单条记录或“删除所有历史”。删除全部历史时也会清空当前未保存图表，并执行 SQLite checkpoint/vacuum 清理。
- 精简历史记录，不保存逐点电池百分比和原始采集数据；升级时会压缩旧版图表历史。
- 支持英文和简体中文界面。

### 兼容性与限制

- 此版本仅面向 **Apple 芯片（`arm64`）Mac**。
- 上游 Powerflow 的 iOS 设备监控功能在本 Mac 专用版本中默认关闭。
- Bundle identifier 仍为 `Powerflow`，因此安装本版本会替换已有的上游 Powerflow 应用。
- 功耗采集依赖 macOS 私有 API、SMC 和 IOKit；未来 macOS 更新可能改变相关行为。
- 并非所有 Mac/macOS 组合都提供屏幕功耗数据。Powerflow 会显示“不可用”，而不会误报为 `0.0W`。

### 安装预发布版本

1. 从 [Releases](https://github.com/LiMiSi23/wattflow-macos/releases) 下载 Apple 芯片版 `.dmg`。
2. 打开磁盘映像，将 **Powerflow** 拖入“应用程序”文件夹。
3. 从“应用程序”中打开 Powerflow。

当前预发布版本使用 ad-hoc 签名，且**未经过 Apple 公证**。如果 Gatekeeper 阻止首次启动，请打开“**系统设置 → 隐私与安全性**”，找到 Powerflow 的提示，点击“**仍要打开**”，再确认“**打开**”。仅应对从本仓库 Releases 页面下载的文件这样操作；如发布说明提供 SHA-256，请先核对。

### 从源码构建

需要 Apple 芯片 Mac、Xcode Command Line Tools、Node.js 22、pnpm `10.0.0-rc.0` 和稳定版 Rust。构建与测试命令与上方英文说明相同。

### 隐私与本地数据

功耗数据和历史记录保存在应用的本地 SQLite 数据库中，本版本没有实现遥测上传。只有用户主动操作时才会打开 GitHub 链接或导出历史。“删除所有历史”会删除应用管理的全部历史、清空当前图表、截断 SQLite 辅助状态并压缩数据库；应用无法控制外部备份或文件系统快照。

### 许可证与来源

本项目使用 [MIT License](LICENSE)，保留上游 Powerflow 的版权声明，并标明修改版本的版权。上游仓库和基础提交信息见 [NOTICE.md](NOTICE.md)。
