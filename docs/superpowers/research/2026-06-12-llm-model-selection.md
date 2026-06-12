# LLM 模型选型：本地小模型用于磁盘分类

**日期**：2026-06-12
**结论**：`qwen3.5:0.8b` 是当前最佳选择。

---

## 背景

`disk_organizer` 用 Ollama 本地 LLM 对 unknown 目录/文件做分类（输出 `category` + `purpose` + `safe to delete?`）。需要找一个平衡点：够快、够准、能在本地跑。

## 候选模型

| 模型 | 大小 | 类型 | 备注 |
|------|------|------|------|
| qwen3:0.6b | ~400MB | 标准 | 最小的 Qwen3 |
| qwen3.5:0.8b | ~530MB | 标准 | Qwen3.5 系列最小 |
| Qwen3-0.6B-Distilled | ~400MB | 蒸馏 | 从大模型蒸馏 |
| Opus-Distilled (reasoning) | ~530MB | 推理蒸馏 | 模仿 Claude Opus 推理 |
| Qwopus3.5-0.8B | ~530MB | 推理蒸馏 | Qwen3.5 + Opus 推理蒸馏 |

## Benchmark 结果

测试条件：`scan.snapshot.json`，`--top 100 --min-size-mb 50 --llm-samples 10`。本地 Ollama、无 GPU、16 核。

| 模型 | 耗时 | 单请求 | 质量 |
|------|------|--------|------|
| **qwen3:0.6b** | 89s | ~2s | 描述偏泛化（大量 "build artifacts"）但无害 |
| Qwen3-0.6B-Distilled | 104s | ~2s | 类似 qwen3:0.6b |
| **qwen3.5:0.8b** | 189s | ~2s | 最精确（识别 LaTeX、NVIDIA CUDA、Android SDK） |
| Opus-Distilled | 1434s | ~16s | 推理模型浪费，无明显质量提升 |
| Qwopus3.5-0.8B | — | ~4-60s | 放弃（见下） |

## Qwopus 深度分析

推理蒸馏模型（如 Qwopus、DeepSeek-R1）在权重层面固化了 `<think>...</think>` 推理流水线。即使 prompt 要求"no thinking"，模型仍然输出 think 块。

### 问题

1. **单请求慢**：即使没有实际推理内容（空 `<think>`），也要走完整流水线。原始 `num_predict` 限制下 ~29s/请求。
2. **对 prompt 长度敏感**：极简 prompt（一行规则）→ 4s 但分类质量差。加回安全规则 → think 块爆炸，60s 超时。
3. **不可控**：`disable_thinking`、`temperature`、`num_predict` 都无法抑制 think 行为。

### 尝试过的优化（失败）

| 尝试 | 效果 |
|------|------|
| JSON Schema 约束解码 | 极慢（GGUF 模型的 grammar 解码有性能问题） |
| `/api/generate` 替代 `/api/chat` | 速度相同 |
| 极简 system prompt | 4s 但不遵守安全规则（说源码可删） |
| 带规则 prompt | 60s 超时，think 块膨胀 |
| `strip_think()` 客户端解析 | 解析正确但模型本身太慢 |

### 根本原因

推理蒸馏模型的设计目标是一次性深思熟虑的问答，不是高吞吐批量分类。`<think>` 块不是可通过 prompt 跳过的模板层，而是权重级别固化的推理路径。

## Takeaways

1. **规则引擎优先**：尽可能多的目录通过规则命中（`AppData`, `Program Files`, `WinSxS`），LLM 只处理 truly unknown 的。
2. **标准 instruct 模型 > 推理蒸馏模型**：分类任务不需要 chain-of-thought。推理模型的开销是纯浪费。
3. **`qwen3.5:0.8b`** 是最佳平衡点：
   - 2s/请求，可接受
   - 描述质量最高（能根据上下文推断具体用途）
   - 530MB，本地轻松跑
4. **不要用 JSON Schema 约束解码**：GGUF 模型的 grammar 解码性能差。纯文本 + 客户端解析更好。
5. **`/api/generate` 优于 `/api/chat`**：更少的协议开销，但差别不大。
6. **本地模型=不限 token**：不用设置 `num_predict`，靠 HTTP 超时控制即可。

## 相关代码

- `src/enrich/llm.rs` — LLM 客户端（`/api/generate` + `strip_think` + `extract_category_purpose`）
- `src/enrich/mod.rs` — enrichment 调度器（cwnd 自适应并发）
- `scripts/bench_models.ps1` — 模型 benchmark 脚本
- `scripts/setup_ollama.ps1` — 一键安装 Ollama + 模型
