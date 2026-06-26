# LLM 推测功能改进计划

> ⚠️ **历史计划（部分已落地，部分已过时）**：创建于 2026-06-12，描述的是 Ollama 双后端时代的
> 改进项。后端已统一到 llama.cpp（单 server，见子项目 A），"双后端统计"等表述不再适用。
> **当前架构见 [`docs/ARCHITECTURE.md`](ARCHITECTURE.md)**；本文仅作规划历史留存。
>
> 创建：2026-06-12 | 基于 `402cdbb`

---

## 一、当前状态

### 强项
- 三阶段流水线（Catalog → Heuristic → LLM）
- JSON Schema 结构化输出 + fallback 解析
- TCP Vegas 拥塞控制 + Selective Repeat（483/483 全成功）
- 可观测性完整（probe log、双后端统计、磁盘+stderr 双输出）
- thinking 控制（单分类 + 最终报告均已开启）

### 瓶颈
| # | 问题 | 优先级 | 影响 |
|---|---|---|---|
| 1 | System prompt 仅 3 条规则，无 few-shot | P0 | 分类准确率不稳定 |
| 2 | 传给 LLM 的上下文字段太薄 | P0 | LLM 缺少目录大小、内容统计 |
| 3 | `analyze_directory_contents` 产出未传入 LLM | P1 | 已有的好数据没利用 |
| 4 | 文件分类仅限 Heuristic 来源 | P2 | Catalog 判错无纠正 |
| 5 | 最终报告用小模型 | P2 | 洞察浅，num_predict=600 限制输出 |

---

## 二、P0-1：重写 System Prompt

### 当前（13 行）
```
"Classify the directory. You MUST output valid JSON matching the schema.
 Rules: C:\\Users\\X->keep. project(src/,Cargo.toml,.git/)->keep.
 cache/node_modules/venv/build/target/dist->safe delete. Be specific."
```

### 改进要点
1. **语言**：全程中文，明确要求输出中文 classification 标签
2. **Windows 特有路径规则**：
   - `C:\Users\<name>\` → keep（用户根目录）
   - `C:\Users\<name>\AppData\Local\Temp\` → safe
   - `C:\Users\<name>\.codebuddy\` → keep（IDE 用户数据）
   - `C:\ProgramData\` → keep
   - `C:\Windows\` → keep
3. **按 Risk 分级**而不是二分类：
   - 明确哪些是 safe delete、哪些是 caution/review、哪些是 keep
4. **Few-shot examples**（3-5 个典型场景的完整 JSON 输出示例）
5. **文件分类 prompt 同样重写**（当前只有 3 行）

### 文件清单
- `src/enrich/llm.rs` — `system_prompt_dir()` 和 `system_prompt_file()`

---

## 三、P0-2：丰富 LLM 传入上下文

### 3.1 目录 prompt 增加字段

当前传给 LLM 的内容：
```
Directory: {dir_path}
Ancestor context: {ctx}
Sample contents: {names...}
```

改进后增加：
```
Directory: C:\Users\dongm\AppData\Local\Temp
Ancestor context: profile 'dongm'
Size: 1.2 GB
Total items in scan: 3,847 files + 142 subdirs (4 levels deep)
Top extensions: .tmp(x2300), .log(x800), .etl(x500), .dmp(x12)
Subdir stats: 142 subdirs; largest: chromium/(321 items)
Sample contents: tmp0001.tmp, log_2026.etl, crash.dmp, ...
```

**需要新增字段**：
- `size_mb: u64` — 目录的物理大小
- 顶级扩展名分布（已经在 `analyze_directory_contents` 里算好了）
- 子目录数量和最大子目录

### 3.2 文件 prompt 增加字段

当前：
```
File: {path}
Extension: .{ext}
Parent directory: {parent}
Ancestor context: {ctx}
Sibling files: {names...}
```

改进后增加：
```
File: C:\Users\dongm\Downloads\setup.exe
Size: 142.3 MB
Extension: .exe
Parent directory: Downloads/
Ancestor context: profile 'dongm'
Sibling files: document.pdf, image001.jpg, ...
```

**需要新增字段**：`size_mb: u64` — 文件大小。

### 3.3 实现方案

需要改 `WorkItem` / `WorkKind` 增加字段，`collect_work()` 填充。

```rust
// mod.rs
enum WorkKind {
    Dir {
        samples: Vec<String>,
        ancestor_context: Option<String>,
        size_mb: u64,          // NEW
        content_summary: String, // NEW — from summarize_children()
    },
    File {
        ext: String,
        parent_dir: String,
        siblings: Vec<String>,
        ancestor_context: Option<String>,
        size_mb: u64,          // NEW
    },
}
```

### 文件清单
- `src/enrich/mod.rs` — `WorkKind`、`collect_work()`
- `src/enrich/llm.rs` — `summarize_dir()`、`summarize_file()` 的 prompt 构建
- `src/enrich/content.rs` — `summarize_children()` 复用

---

## 四、P1：利用 `analyze_directory_contents` 产出

### 现状
`content.rs::summarize_children()` 已经做得很好：
- 统计文件扩展名分布（Top-4 by size）
- 统计文件数量
- 统计子目录数量和最大子目录
- 检测 `.git/` 标记 git repo

但它**只用于 Catalog 已分类的目录**，Unknown 目录（要走 LLM）的同一份数据没有传给 LLM。

### 改进
在 `collect_work()` 收集 Unknown 目录时，同步调用 `summarize_children()`，把摘要字符串存入 `WorkKind::Dir.content_summary`，然后传给 `summarize_dir()` 的 prompt。

**注意**：`summarize_children()` 已经在 `enrich_items` 的后期对所有 Item 调用了，这里需要在 `collect_work()` 阶段提前调用一次（仅对 Unknown 目录）。性能影响可忽略——只是 HashMap 查找。

### 文件清单
- `src/enrich/mod.rs` — `collect_work()` 中调用 `summarize_children()`

---

## 五、P2-1：最终报告支持大模型

### 现状
最终报告用同一个模型（`qwen3.5:0.8b`），`num_predict=600`。0.8B 的 reasoning 能力有限。

### 改进
在 `LlmConfig` 中增加可选字段：
```rust
pub struct LlmConfig {
    // ... existing fields ...
    /// Optional larger model for final report (falls back to `model` if None).
    pub report_model: Option<String>,
}
```

如果配置了 `report_model`，最终报告使用该模型并增加 `num_predict` 到 1500+。

### 文件清单
- `src/enrich/mod.rs` — `LlmConfig` + 报告生成段
- `src/enrich/llm.rs` — `summarize_report()` 接受 model 参数

---

## 六、P2-2：文件分类范围扩展

### 现状
```rust
if it.source == Source::Heuristic {
    // LLM re-analyze
}
```
只有 Heuristic 来源的文件才交 LLM 重分析。

### 改进
增加对 **Unknown 来源 + 大文件**的 LLM 分析。Catalog 命中的仍跳过（信任 catalog）。

建议阈值：物理大小 > 50MB 且 Source != Catalog 的文件。

```rust
} else {
    if it.source == Source::Heuristic 
        || (it.source == Source::Unknown && it.physical_size > 50_000_000) 
    {
        // LLM analysis
    }
}
```

---

## 七、完整 Task List

| 序号 | 任务 | 优先级 | 预计改动量 |
|---|---|---|---|
| T1 | 重写 `system_prompt_dir()` + `system_prompt_file()` | P0 | ~60 行 |
| T2 | `WorkKind` 增加 `size_mb` + `content_summary` 字段 | P0 | ~10 行 |
| T3 | `collect_work()` 填充新字段 | P0 | ~15 行 |
| T4 | `summarize_dir()` / `summarize_file()` 更新 prompt 模板 | P0 | ~20 行 |
| T5 | `collect_work()` 中对 Unknown 目录调用 `summarize_children()` | P1 | ~5 行 |
| T6 | `LlmConfig` 增加 `report_model` 可选字段 | P2 | ~15 行 |
| T7 | 最终报告使用可选大模型 | P2 | ~10 行 |
| T8 | 大文件 LLM 分析范围扩展 | P2 | ~5 行 |

---

## 八、实施顺序

```
Phase 1 (本次): T1 → T2 → T3 → T4     [P0: prompt + 上下文]
Phase 2 (下次): T5                      [P1: content summary 复用]
Phase 3 (后续): T6 → T7 → T8            [P2: 范围扩展]
```

Phase 1 是核心——做完后分类质量应该有质的飞跃。执行 Phase 1 的 4 个 task，估计 ~100 行净增，涉及 3 个文件。

---

## 九、风险与注意事项

1. **Prompt 长度增长**：每次 LLM 请求的输入 token 会增加 200-400 tokens。以 2 req/s 的速度，每小时多消耗 ~2.8M tokens，在当前硬件上可忽略。
2. **`summarize_children()` 提前调用**：确保索引在 `collect_work` 时已完全构建。
3. **Few-shot 示例**：示例路径应包含 `C:\Users\dongm\` 前缀，避免 LLM 把示例当成真实输出。只描述类别。
4. **测试**：`WorkKind` 字段变更会影响 `do_summarize()` 的模式匹配，需要更新测试。
