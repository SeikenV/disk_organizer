# disk_organizer — 架构（现状 / Living Architecture）

> 最后更新：2026-06-26 · 状态：**与代码同步的权威文档**
>
> 本文描述**当前实际实现**。2026-06-08 的功能/架构/实现流程三篇是开发前的设计稿，
> 多处已与现状不符（见各文件顶部的「已被取代」提示），仅作历史记录保留。
> 设计演进的权威记录在 `docs/superpowers/specs/` 下的后续设计稿：
> - `2026-06-22-backend-unification-llamacpp-design.md`（Ollama → llama.cpp）
> - `2026-06-26-m3-video-description-design.md`（M3 视频描述）

## 1. 总览

分层 + 渐进增强流水线：扫描层用 **NTFS MFT** 秒级产出"大小骨架"，分类层用确定性规则切割，
增强层用**本地 llama.cpp 模型**为未知项补"用途/风险"，视频描述为独立能力。

```
┌──────────────────────────────────────────────────────────────┐
│ 呈现层   CLI（stdout JSON 清单）          ui/（外部 App，预留）  │
├──────────────────────────────────────────────────────────────┤
│ 增强层   enrich/  ── 自管理 llama-server（文本 + 视觉）          │
│          backend(选后端) server(进程生命周期) client(OpenAI兼容)│
│          lifecycle(健康/预热) llm(文本提示) video+frames(视频)   │
│          orchestrator(TCP式拥塞控制) content(目录体检)          │
├──────────────────────────────────────────────────────────────┤
│ 分类层   classify/  catalog(知识库) cut(递归切割) tree           │
├──────────────────────────────────────────────────────────────┤
│ 数据层   model(Item/Risk/Source)  scan/index(FRN 索引)+aggregate│
├──────────────────────────────────────────────────────────────┤
│ 扫描层   scan/  volume(读卷 MFT) mft(解析记录) snapshot(快照)    │
├──────────────────────────────────────────────────────────────┤
│ 动作层   act/  delete(回收站) select                            │
└──────────────────────────────────────────────────────────────┘
外部依赖：llama-server（项目自管理，非 Ollama） · ffmpeg/ffprobe（子进程） · 回收站 API
```

仓库为 Cargo workspace：**`engine/`**（headless 引擎，crate 名 `disk_organizer`）+ **`ui/`**
（独立工具链的外部 App，**不在 cargo workspace 内**，当前为预留）。

## 2. 技术栈（现状）

| 维度 | 选择 | 说明 / 与原设计的差异 |
|------|------|----------------------|
| 语言 | **Rust** | 不变 |
| 磁盘计量 | **直接解析 NTFS MFT**（`mft` crate + 自研卷读取） | ⚠️ 原设计是 jwalk 并行遍历；实测为精确计量与性能改用 MFT（需管理员权限） |
| 硬链接/重解析点 | 按 MFT 记录与扩展记录精确归并 | WinSxS 等不虚高 |
| 本地推理后端 | **llama.cpp `llama-server`（项目自管理进程）** | ⚠️ 原设计是 Ollama；已移除 `ollama-rs`/`tokio` |
| 后端选择 | 运行时 **CUDA → Vulkan → CPU**，逐个尝试启动并回退 | `enrich/backend.rs` + `server::start_with_fallback` |
| 文本模型 | **Qwen3.5-0.8B (UD-Q4_K_XL)** | ⚠️ 原设计 Qwen2.5-1.5B；Q4 更快且质量达标 |
| 视觉模型 | **SmolVLM2-500M-Video-Instruct + mmproj** | ⚠️ 原设计 Qwen2.5-VL-3B；SmolVLM2 为视频原生、token 高效 |
| 结构化输出 | `schemars` 派生 JSON Schema → `response_format: json_schema`；`enable_thinking:false` | llama.cpp 等价于 Ollama 的 `format=json` + `think:false` |
| HTTP | `reqwest`（blocking），**统一走 `client::local_client()`（`.no_proxy()`）** | 绕开 Windows 系统代理对 127.0.0.1 的劫持 |
| 抽帧 | **ffmpeg**（快速 `-ss` 关键帧定位）+ **ffprobe**（时长/帧率） | 蒙太奇用 ffmpeg `tile`，无需额外图像库 |
| 知识库 | BleachBit CleanerML 数据 + 自有补充 | 不变（`classify/catalog.rs`） |

> 工具二进制由 `scripts/setup_tools.ps1` 拉取到 **gitignored 的 `tools/`**：
> `tools/llamacpp/<cpu|cuda|vulkan>/llama-server.exe`、`tools/models/*.gguf`；
> 加 `-Video` 还会拉取 `tools/ffmpeg/{ffmpeg,ffprobe}.exe` 与 SmolVLM2。

## 3. 模块结构（`engine/src/`）

```
main.rs            CLI 解析 + 流水线编排 + 诊断短路（--size-audit / --describe-video[s-from]）
lib.rs             导出 scan/classify/act/enrich/model/consts/format/report
consts.rs          阈值、重试、拥塞窗口、worker 上限等集中常量
model.rs           Item / Risk / Source / RawRecord（核心数据类型）
format.rs          人类可读体积/时长格式化
report.rs          ReportFile（增强报告落盘）

scan/
  volume.rs        以管理员权限读取卷的 $MFT 原始字节
  mft.rs           解析 MFT 记录（含扩展记录的 $DATA 体量归并）+ size_audit
  index.rs         build_index：FRN → 记录索引，重建父子路径
  aggregate.rs     子树体量/文件数聚合
  paths.rs         路径拼装工具
  snapshot.rs      扫描快照 save/load（JSON，免管理员复跑）

classify/
  catalog.rs       已知目录知识库与匹配；is_container 区分"容器/叶子"目录
  cut.rs           递归切割成 Item，贴规则分类（Rule/Heuristic/Unknown）
                   叶子目录(缓存/系统，如 npm-cache/WinSxS)整体作为一个 Item 且不再下钻；
                   容器目录(用户数据：用户主目录及 Downloads/Videos/Pictures/Documents/Desktop)
                   会下钻，把内部大文件/子目录各自暴露成 Item（残余部分仍带容器标签）
  tree.rs          切割辅助树结构

enrich/                         （增强层 —— 见 §5）
  backend.rs       Backend{Cuda,Vulkan,Cpu} + select_backend + default_prefs
  server.rs        ★生命周期：LlamaServer（spawn/health/Drop-kill）、ServerConfig、start_with_fallback、上下文分片规则
  client.rs        ★传输：OpenAI 兼容 chat、local_client(no_proxy)、build_json_body、build_image_request(图像)
  lifecycle.rs     health_check（GET /health）、preload_model（预热）
  llm.rs           文本提示：summarize_dir/file/report + Schema + 解析（只用 endpoint，不管生命周期）
  video.rs         ★VisionSession（持有视觉 server）+ describe（仅提示）+ VideoContentGuess + is_video_path
  frames.rs        ffprobe 探测 + ffmpeg 蒙太奇 + 纯函数(frames_to_sample/montage_grid)
  orchestrator.rs  CwndCtl（TCP Vegas 式拥塞控制）+ run_supervisor
  content.rs       analyze_directory_contents（已知目录"名实体检"统计）
  mod.rs           enrich_items（文本增强主流程，单 server）+ LlmConfig + 对外 re-export

act/
  delete.rs        移入回收站（trash crate）
  select.rs        选择辅助
```

## 4. 运行时流程

### 主流程（分类 + 文本增强）
```
disk_organizer C --llm [--backend ...] [--llm-* ...]
  1. 取记录：读 MFT（volume→mft）或 --from-snapshot 载入
  2. build_index → aggregate → cut（规则切割，秒级出"规则版"清单）
  3. 按 LLM-eligible 数量截断到 --top
  4. content::analyze_directory_contents（已知目录体检统计）
  5. 若 --llm：enrich_items —— 启动 1 个 llama-server（CUDA→Vulkan→CPU 回退），
     预热模型，按 --parallel 槽位并发，用 TCP 式拥塞控制喂请求，
     未知目录/启发式文件逐个 summarize，最后 summarize_report 汇总；
     无可用后端则记录提示并降级为"纯规则版"
  6. stdout 输出 Item JSON（pretty）；增强报告写入 enrichment_report_*.log
```
扫描需**管理员权限**（原始读卷）。`--from-snapshot` 用快照复跑，免管理员。

### 视频描述（独立诊断短路）
```
disk_organizer --describe-video <path>            # 单个，输出一个 guess 对象
disk_organizer --describe-videos-from <items.json># 批量，输出 guess 数组
  → VisionSession::start（启动 1 个带 --mmproj 的视觉 server，加载一次）
  → 对每个视频：ffprobe 时长 → frames_to_sample(clamp(0.1%,4,16))
                → ffmpeg 快速定位抽帧 → tile 蒙太奇（默认 512px 缩放）
                → client::build_image_request → chat → 解析为 VideoContentGuess
  → session Drop 时关闭 server
```
辅助脚本 `scripts/predict_videos.ps1`：从增强结果 JSON 里挑出视频，单次 `--describe-videos-from`
预测全部并与文本分类合并成报告。

## 5. 增强层设计原则（生命周期 ⟂ 提示）★

这是本层的核心约束，务必遵守：

- **启停 llama-server、加载/卸载模型，是生命周期模块的唯一职责。**
  - `server.rs` 的 `LlamaServer` 拥有进程：`start` 时拉起并等待 `/health`，`Drop` 时 kill。
  - 文本路径：`enrich_items` 在一次运行里**只启动一个 server**，循环复用，结束即 Drop。
  - 视频路径：`VisionSession` 持有一个 server，`describe` 多个视频复用它——**一批一个 server，绝不一项一个**。
- **其余模块只负责"撰写 prompt 调用已启动的 server"。**
  - `llm.rs` 的 `summarize_*` 与 `VisionSession::describe` 都只接收/使用 `endpoint`，从不自己启停 server。
- **后端选择与回退**：`start_with_fallback(prefs, cfg)` 按偏好逐个尝试启动，失败回退下一后端。
- **上下文分片规则**：llama-server 把 `-c` 总上下文均分给 `--parallel` 个槽位，故
  `每槽 = 总 / parallel` 必须 ≥ 典型 prompt+输出（约 2048）；启动器用 `parallel × per_slot_ctx` 计算总值。
- **回环必须绕代理**：所有对 127.0.0.1 的请求走 `client::local_client()`（`.no_proxy()`），
  否则 Windows 系统代理（如本机 Clash）会劫持回环请求导致健康检查超时。
- **结构化输出**：请求带 schemars 生成的 JSON Schema 约束，且 `chat_template_kwargs:{enable_thinking:false}`
  （关闭推理段，否则内容落到 thinking 而非 content）。

## 6. 数据模型（`model.rs`）

```rust
struct Item {              // 呈现给用户、可勾选删除的单元；字节不重叠
    frn: u64,
    path: PathBuf,         // 绝对路径，如 C:\Users\me\AppData\Local\npm-cache
    is_dir: bool,
    physical_size: u64,    // 物理体量（硬链接已去重）
    file_count: u64,
    category: String,      // 分类（规则或 LLM 给出）
    purpose: String,       // 人类可读用途
    risk: Risk,            // Safe | Caution | System | Unknown —— 永远由规则决定
    source: Source,        // Rule | Heuristic | LLM | Unknown
}
```
视频描述的输出是独立类型 `VideoContentGuess { summary, category(VideoCategory), confidence }`，
当前**不**参与 `Item` 风险判定（M3 接入主流程为后续工作）。

## 7. 删除安全（零误删红线）

1. 默认**移入回收站**（`trash` crate），不永久删除。
2. 风险等级只由**确定性规则**决定；LLM/视觉**绝不**把任何项判为"可安全删除"。
3. 引擎只输出"带解释的清单"；是否删除 100% 由用户决定。

## 8. 里程碑现状

| 里程碑 | 内容 | 状态 |
|--------|------|------|
| M1 | MFT 扫描 + NTFS 精确计量 + 规则切割 + CLI 清单 + 快照 | ✅ 已完成（已合并 main） |
| M2 | 文本 LLM：未知目录摘要 + 已知目录体检 | ✅ 已完成 |
| 子项目 A | 后端统一 Ollama → llama.cpp（多后端 + 回退） | ✅ 已完成 |
| M3（子项目 B，核心） | `--describe-video` 视频内容描述（SmolVLM2 + ffmpeg） | ✅ 核心引擎已完成 |
| M3 接入 | "大 + 无意义命名"视频触发、延后队列、并入 Item 风险映射 | ⏳ 延后（独立周期） |
| M4 | GUI（复选框 + treemap），复用引擎 | ⏳ 后续（`ui/` 已预留） |

## 8.5 语言选项与 Web 搜索接口（reserved）

- **语言选项 `--language <code|name>`**：分类按原有（已调优的）提示词产出后，再跑一轮
  **批量翻译**把每个 item 的 `category`/`purpose` 译成目标语言；最终报告直接用目标语言生成。
  不设则保持模型默认语言。译文是展示层转换，**不改变 risk**。实现要点：
  - **用全名而非代码**：内部把 `en`/`ja`/`zh` 展开成 `English`/`Japanese`/`Chinese`
    （`language_name`）——小模型不认 ISO 代码，认全名（实测这是关键修复）。
  - **按字形跳过**（`needs_translation`）：已经是目标字形的文本不再翻译，避免把正确的
    英文目录文案又翻回中文，也减少调用。
  - **批量 + 回退**：每 ~10 个 item 一次调用，校验数量，失败回退逐项/保留原文。
  - ⚠️ **模型限制**：默认 Qwen3.5-0.8B 实测约 85–88% 条目能正确翻成目标语言，少数会
    残留源语言；要 100% 保真请用更大的文本模型（`--llm-model-path`，同 VLM 的 2.2B 建议）。
    （真实文件名等专有名词有意保留不译。）
- **Web 搜索接口（预留，GUI 之后再实现）**：`--web-search` 旗标已占位（当前为 no-op，
  仅告警）。未来实现的接入点在 `summarize_dir`/`summarize_file` 组装提示词处，与
  `ancestor_context`/`content_summary` 并列注入一段"厂商/产品"外部上下文。约束：
  本项目是 **cost-free-first**（非 privacy-first），但外部上下文只能作为**不可信提示**
  影响 `purpose`，**绝不参与 risk 判定**（与 LLM 同一红线）；且只针对 Program Files/
  厂商目录的未知项，避免把噪声喂给小模型造成更差的"自信误判"。

## 9. 配置（现状：CLI 参数，无配置文件）

原设计的 `llm.endpoint/text_model/...` 配置块已被 CLI 参数取代，例如：
- 扫描：`--top`、`--min-size-mb`、`--from-snapshot`、`--save-snapshot`、`--size-audit`
- 文本 LLM：`--llm`、`--backend cuda|vulkan|cpu`（可重复）、`--llm-model-path`、`--tools-dir`、
  `--llm-parallel`、`--llm-per-slot-ctx`、`--llm-ngl`、`--llm-port`、`--llm-samples`
- 视频：`--describe-video <path>`（可重复）、`--describe-videos-from <json>`、`--vlm-model-path`、
  `--vlm-mmproj-path`、`--ffmpeg-dir`、`--vlm-port`、`--vlm-frame-rate`、`--vlm-min-frames`、
  `--vlm-max-frames`、`--vlm-downscale`（0=关闭，默认 512px）
- 语言/预留：`--language <code>`（第二轮翻译，见 §8.5）、`--web-search`（预留 no-op）
- 删除：移入回收站（默认）
```
# 拉取工具（CPU + 文本模型）；加 -Video 拉 ffmpeg + 视觉模型
./scripts/setup_tools.ps1            # 文本
./scripts/setup_tools.ps1 -Video     # 文本 + 视频
```
