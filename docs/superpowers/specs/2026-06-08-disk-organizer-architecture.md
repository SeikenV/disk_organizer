# disk_organizer — 架构文档（Architecture）

> 版本：2026-06-08 · 状态：设计稿待评审 · 配套：功能文档、实现流程文档

## 1. 总览

分层 + 渐进增强流水线。扫描层快速产出"骨架"，分类/AI 层按阶段把"血肉"流式填上。

```
┌─────────────────────────────────────────────────────────────┐
│ 呈现层  CLI Presenter / Selector / Deleter   (M4: GUI)        │
├─────────────────────────────────────────────────────────────┤
│ 编排层  Enrichment Queue (任务队列 + 状态机 + 优先级/并发)     │
├───────────────┬───────────────┬───────────────┬──────────────┤
│ 切割/分类引擎  │ 文本 LLM 编排  │ 视觉编排(延后)  │ 已知目录知识库 │
│ Cut & Classify│ Text LLM Orch.│ Vision Orch.  │ Catalog       │
├───────────────┴───────────────┴───────────────┴──────────────┤
│ 数据模型  Node Tree / DataTree（大小聚合、增强状态）           │
├─────────────────────────────────────────────────────────────┤
│ 扫描层  Parallel Scanner（jwalk）+ NTFS 处理                   │
├─────────────────────────────────────────────────────────────┤
│ 持久化  Snapshot Store（扫描快照缓存 JSON）                    │
└─────────────────────────────────────────────────────────────┘
外部依赖： Ollama(HTTP)  ·  ffmpeg(subprocess)  ·  回收站 API
```

## 2. 技术栈决策

| 决策 | 选择 | 理由 | 备选（被否） |
|------|------|------|-------------|
| 核心语言 | **Rust** | 性能优先 + 多核并行是核心目标；单二进制、无 GC 停顿；本项目意在探索"Rust + 本地小模型"组合 | Python（AI 编排更易，但遍历性能弱、不契合"追求性能"）；Rust 扫描 + Python 编排（двух语言增复杂度） |
| 并行遍历 | **jwalk** | 成熟的并行目录遍历；`dua-cli`/`parallel-disk-usage` 均基于它 | 自写线程池（重复造轮子） |
| 树模型参考 | **parallel-disk-usage** 的 `DataTree` 思路 | 现成的大小聚合树结构 | 从零设计 |
| 已知目录知识库 | **BleachBit CleanerML** 数据 + 自有补充 | 社区维护的清理位置目录，现成 | 纯自己枚举（量大易漏） |
| 本地推理 | **Ollama**（HTTP API） | 模型热插拔、`format=json` 约束解码、部署省事 | llama.cpp 直连（更底层，GBNF 语法约束，后续可换） |
| 文本模型 | **Qwen2.5-1.5B-Instruct** | 小尺寸里指令遵循/结构化输出最强；多语言（中文解释） | Llama3.2-1B、Gemma2-2B |
| 视觉模型 | **Qwen2.5-VL-3B** 或 **moondream2(1.8B)** | 原生视频抽帧理解；中文 | LLaVA 系 |
| 抽帧 | **ffmpeg**（子进程） | 行业标准 | 自带解码器（复杂） |

> ⚖️ **许可提醒**：jwalk/pdu/Ollama 宽松许可；**BleachBit CleanerML 为 GPL**——若将来开源发布且直接沿用其数据，发布物可能需 GPL。自用无碍。

## 3. 组件分解

每个组件遵循"单一职责 + 明确接口 + 可独立测试"。

### C1. Parallel Scanner（扫描层）
- **职责**：并行遍历给定根路径，产出原始节点流（路径、大小、属性、文件 ID）。
- **接口**：`scan(root, opts) -> Stream<RawEntry>`。
- **依赖**：jwalk、Windows 文件 API。
- **关键处理**见 §5（NTFS）。

### C2. Node Tree / DataTree（数据模型）
- **职责**：把 RawEntry 流聚合成树，计算每个目录的子树总大小与文件数。
- **接口**：`build(stream) -> Tree`；`Tree::top_n(threshold)`、`Tree::walk()`。
- **依赖**：C1。

### C3. Catalog（已知目录知识库）
- **职责**：保存"已知目录模式"，提供匹配与元数据。
- **数据条目**：`{ pattern(含 %LocalAppData% 等变量), purpose, risk, expected_signature }`。
- **接口**：`match(path) -> Option<CatalogEntry>`。
- **来源**：CleanerML 解析 + 自有 YAML 补充。
- **依赖**：无（纯数据 + 匹配器）。

### C4. Cut & Classify Engine（切割/分类引擎）⭐核心
- **职责**：在节点树上执行**递归切割**（§6），把子树打包成"条目"，并贴上规则分类。
- **接口**：`cut(tree, catalog, opts) -> Vec<Item>`，每个 `Item` 带 `enrichment_status`。
- **依赖**：C2、C3。

### C5. Text LLM Orchestrator（文本 LLM 编排）
- **职责**：①未知大目录"用途摘要"（抽样 K 文件→一句总结）②已知目录"名实体检"③按需逐文件。
- **接口**：`summarize_dir(item) -> Labeling`、`verify_dir(item, signature) -> VerifyResult`、`analyze_files(files) -> Vec<Labeling>`。
- **约束**：Ollama `format=json`，输出受 `{category∈enum, purpose, confidence}` 约束。
- **依赖**：Ollama、Catalog（拿预期签名）。

### C6. Vision Orchestrator（视觉编排·延后）
- **职责**：对"大 + 文件名无意义"的视频，ffmpeg 抽帧 → VLM 猜内容。
- **接口**：`guess_media(file) -> MediaGuess`。
- **触发**：仅进延后队列（最低优先级）。
- **依赖**：ffmpeg、Ollama(VLM)。

### C7. Enrichment Queue（编排层）
- **职责**：管理"增强任务"的状态机、优先级与并发；驱动结果流式更新到呈现层。
- **优先级**：规则(同步) > 文本 LLM > 视觉。
- **接口**：`enqueue(task)`、`subscribe() -> Stream<ItemUpdate>`。
- **依赖**：C5、C6。

### C8. Snapshot Store（持久化）
- **职责**：扫描结果与已得标注落盘（JSON），支持"重开即用、增量增强"。
- **接口**：`save(tree+items)`、`load() -> Option<...>`。

### C9. CLI Presenter / Selector / Deleter（呈现层）
- **职责**：渲染清单、处理 `open/why/select`、汇总确认、移入回收站。
- **接口**：命令循环；`delete(selection) -> Report`（默认回收站）。
- **依赖**：C7（订阅更新）、回收站 API。

## 4. 数据模型

```rust
struct Node {
    path: PathBuf,
    kind: Kind,            // File | Dir | Media
    size: u64,             // 子树聚合（已去重硬链接）
    file_count: u64,
    file_id: Option<FileId>, // (volume_serial, file_index) 用于硬链接去重
    children: Vec<Node>,
}

struct Item {              // 切割后呈现给用户的单元
    node: NodeRef,
    classification: Classification,
    status: EnrichmentStatus,
    verification: Option<VerifyResult>,
}

struct Classification {
    category: String,      // 受 enum 约束
    purpose: String,       // 人类可读
    risk: Risk,            // Safe|Caution|System|Unknown —— 规则优先
    confidence: f32,
    source: Source,        // Rule | Llm | Vision
}

enum EnrichmentStatus { Pending, RuleLabeled, LlmLabeled, VisionLabeled, Verified }
```

## 5. NTFS 关键处理（自研补齐，无现成 Rust 库）

| 问题 | 处理 | 不处理的后果 |
|------|------|-------------|
| **硬链接**（WinSxS 重灾） | 按 `FileId`(卷序列+文件索引) 去重，同一实体只计一次 | WinSxS 等虚高数 GB，诊断失真 |
| **重解析点**(junction/symlink) | 检测 `FILE_ATTRIBUTE_REPARSE_POINT`，**不跟随** | 重复计数 + 可能死循环 |
| **长路径 >260** | 使用 `\\?\` 前缀 / 宽字符 API | 深层目录扫不到 |
| **权限/被占用** | 优雅跳过并记录，不中断 | 扫描崩溃 |

## 6. 递归切割算法 ⭐

```
classify(dir):
    entry = catalog.match(dir.path)
    if entry is not None:                # 命中已知目录
        emit Item(dir, label=entry, status=RuleLabeled)
        if dir.size 显著 and entry.expected_signature:
            queue VERIFY(dir, entry.expected_signature)   # 名实体检
        return                           # ★ 不再往下细分
    else:                                # 未知目录
        if dir.size < threshold: return  # 太小，忽略
        loose = []
        for child in dir.children:
            if child.kind == Dir: classify(child)   # 继续下钻，深处可能有已知目录
            else: loose.push(child)
        # 自身仍未识别且够大 → 作为"未知条目"，交文件树 + LLM 摘要
        emit Item(dir, status=Pending)
        queue SUMMARIZE(dir, sample(loose))
        # C 兜底：模型/规则都不认的区域，按大小做祖先/后代去重，避免重叠
```

`threshold`：默认"绝对体量下限（如 ≥100MB）+ Top-N"，二者可配置。

## 7. 模型与约束解码

- 文本：Ollama `chat`，`format` 传 JSON Schema，`category` 用 enum 锁死 → 1.5B 也能稳定结构化输出。
- 视觉：抽 3~5 帧 → 多图输入 → 让模型输出"内容方向 + 置信度"，措辞强调"推测非确证"。
- 模型按阶段加载（文本阶段用 1.5B，视觉阶段切 VLM），内存压力低。

## 8. 模块边界与隔离

- 扫描层对上只暴露 `RawEntry` 流，不关心分类。
- Catalog 是纯数据 + 匹配，可单测、可热更新。
- Cut Engine 不直接调模型——它只产出"需要增强"的任务，交给 Queue；模型不可用时系统仍能给出"规则版"完整结果。
- 呈现层通过订阅 `ItemUpdate` 与内核解耦，故 CLI→GUI 仅换此层。
