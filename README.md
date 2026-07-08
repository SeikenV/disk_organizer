# disk_organizer

> 基于 Windows NTFS MFT 的本地磁盘清理分析器，支持本地 LLM 智能分类与视频内容识别。

`disk_organizer` 直接读取 NTFS 主文件表（MFT），在秒级内完成整盘扫描与精确体积计量，再通过规则 + 本地大语言模型（llama.cpp）为未知目录生成用途说明，帮助你决定哪些文件可以清理。

---

## 特性

- **秒级全盘扫描**：直接解析 NTFS MFT，无需递归遍历文件夹；精确处理硬链接、重解析点、WinSxS 等场景。
- **本地 LLM 增强**：使用自管理的 `llama-server`（llama.cpp），无需 Ollama/网络服务；默认模型 `Qwen3.5-0.8B`，可在笔记本 GPU 上运行。
- **多后端自动回退**：CUDA → Vulkan → CPU，按可用性逐个尝试启动并自动回退。
- **视频内容识别**：基于 `SmolVLM2-500M-Video-Instruct` + ffmpeg 抽帧，描述视频大概内容（独立诊断命令）。
- **安全优先**：LLM 只影响「用途说明」，风险等级由确定性规则决定；删除默认移入回收站，不永久删除。
- **快照复跑**：支持保存/载入扫描快照，分析阶段无需管理员权限。

---

## 技术栈

| 维度 | 选择 |
|------|------|
| 语言 | Rust（Cargo workspace，`engine/`） |
| 磁盘计量 | 直接解析 NTFS MFT（`mft` crate + 自研卷读取） |
| 本地推理后端 | 自管理 llama.cpp `llama-server`（CPU / CUDA / Vulkan 多后端） |
| 文本模型 | Qwen3.5-0.8B-UD-Q4_K_XL |
| 视觉模型 | SmolVLM2-500M-Video-Instruct-Q8_0 + mmproj |
| 抽帧 | ffmpeg / ffprobe |
| 删除 | 回收站（`trash` crate） |

---

## 快速开始

### 1. 环境要求

- Windows 10/11
- 管理员权限（扫描 MFT 需要原始读卷）
- 如需 GPU 加速：NVIDIA/AMD 驱动已安装

### 2. 拉取工具

以管理员身份打开 PowerShell：

```powershell
# 仅文本分类
.\scripts\setup_tools.ps1

# 文本 + 视频描述
.\scripts\setup_tools.ps1 -Video
```

脚本会自动下载：

- `tools/llamacpp/{cpu,cuda,vulkan}/llama-server.exe`
- `tools/models/*.gguf`
- （加 `-Video` 时）`tools/ffmpeg/{ffmpeg,ffprobe}.exe`

### 3. 构建引擎

```powershell
cargo build --release
```

### 4. 扫描并分析

```powershell
# 扫描 C 盘，输出前 40 个大项的 JSON 清单
.\target\release\disk_organizer.exe C

# 启用本地 LLM 增强（自动选择 CUDA/Vulkan/CPU）
.\target\release\disk_organizer.exe C --llm

# 从快照复跑（无需管理员）
.\target\release\disk_organizer.exe --from-snapshot scan.snapshot.json --llm
```

### 5. 视频描述（可选）

```powershell
# 单个视频
.\target\release\disk_organizer.exe --describe-video "C:\Users\me\Videos\demo.mp4"

# 批量：从增强结果 JSON 中挑出所有视频并预测
.\scripts\predict_videos.ps1 -ItemsJson enrichment_report_*.log
```

---

## 项目结构

```
.
├── engine/              # Rust 引擎（Cargo workspace 成员）
│   └── src/
│       ├── scan/        # MFT 读取、解析、索引、聚合
│       ├── classify/    # 规则知识库、递归切割
│       ├── enrich/      # llama-server 生命周期、客户端、拥塞控制、LLM/视频增强
│       ├── act/         # 删除/选择动作
│       └── main.rs      # CLI 入口
├── scripts/             # PowerShell 安装/辅助脚本
├── tools/               # 自管理二进制（gitignored，由 setup 脚本拉取）
├── ui/                  # 外部 UI App 预留目录
└── docs/                # 架构文档与设计稿
```

---

## 常用 CLI 参数

```text
DRIVE                  要扫描的盘符，例如 C
--top <N>              输出前 N 项 [默认: 40]
--min-size-mb <N>      最小输出体积 [默认: 200]
--llm                  启用本地 LLM 增强
--backend <BACKEND>    后端偏好顺序，可重复：cuda / vulkan / cpu
--llm-parallel <N>     llama-server 并发槽位 [默认: 4]
--llm-ngl <N>          GPU 层数 [默认: 999]
--from-snapshot <PATH> 从快照加载，无需管理员
--save-snapshot <PATH> 保存扫描快照
--describe-video <PATH> 描述单个视频内容
--debug                输出详细日志到 logs/
```

完整参数请运行：

```powershell
.\target\release\disk_organizer.exe --help
```

---

## 安全与隐私说明

1. **零误删红线**：引擎只输出「带解释的清单」，是否删除 100% 由用户决定；删除默认移入回收站。
2. **风险等级不由 AI 决定**：`Risk`（Safe / Caution / System / Unknown）只能由确定性规则判定，LLM/视觉模型只用于生成 `purpose` 与 `category`。
3. **完全本地运行**：默认不连接任何云服务；所有推理在本机 `llama-server` 完成。

---

## 文档

- [ARCHITECTURE.md](docs/ARCHITECTURE.md) — 当前架构与实现细节
- [docs/superpowers/](docs/superpowers/) — 设计演进稿与规格说明

---

## 许可证

MIT
