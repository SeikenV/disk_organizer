# 工程实践 Takeaways

**日期**：2026-06-12
**来源**：`disk_organizer` 项目全程开发中的踩坑记录。

---

## 1. cwnd 自适应并发调参：理论简单，实践全是坑

本地 LLM 批量分类的核心难题：如何用自适应并发窗口（cwnd）在 482 个请求间动态平衡吞吐量。

### 问题 1.1：cwnd 自然衰减（CWND_LINEAR_DECR = 1 → 0）

**现象**：cwnd 从 8 随时间自然降到 5。旧逻辑中每次 probe 都线性递减 cwnd，导致即使系统空闲、SRTT 正常，cwnd 也在下降。

**根因**：线性衰减（CWND_LINEAR_DECR）与增长逻辑竞争，衰减粒度过粗。

**修复**：将 `CWND_LINEAR_DECR` 从 1 改为 0，完全放弃线性衰减，只靠 SRTT 阈值触发 shrink。

### 问题 1.2：cwnd 卡在 5 不动

**现象**：cwnd 涨到 5 后不再增长。`SRTT_RECOVER_THRESHOLD` 当初设 0.7，但 SRTT 基线 ~1800ms，recover 门槛 = 1260ms，而实际 SRTT ~1500ms，永远达不到 recover 门槛。

**根因**：阈值算错了——以基线为分母算比例，但 SRTT 是一个绝对延迟值，比例阈值应基于 SRTT/基线的比值，而非直接乘以基线。

**修复**：`SRTT_RECOVER_THRESHOLD` 从 0.7 调至 0.85。

### 问题 1.3：steady GROW 阶段 peak_cwnd 遗漏更新

**现象**：在 steady 阶段 GROW 决策时，cwnd 在执行端成功增长，但 peak_cwnd 未更新。

**根因**：grow 逻辑有显式 CAS 更新 peak_cwnd，但 steady GROW 分支漏掉了这个操作。两个分支行为不一致。

**修复**：在 steady GROW 分支添加同样的 lock-free CAS 更新。

### 问题 1.4：probe log 中 infl > cwnd 的显示 bug

**现象**：probe 日志中 `infl`（in-flight）有时大于 `cwnd`，数值矛盾。

**根因**：`infl` 和 `cwnd` 的读取不在同一原子时刻。先读 `infl`，再读 `cwnd`，中间有其他线程释放/获取。

**修复**：在同一临界区内快照两个值（`snapshot_inflight` + `current_cwnd` 同时读取）。

### Takeaway

> 自适应并发不是调一调常数就能工作的。SRTT 的绝对/相对语义、CAS 更新的遗漏分支、日志快照的原子性，每层都可能引入隐蔽 bug。**把决策函数拆成可单独测试的纯函数，用大量单元测试覆盖边界**是唯一可靠的保证。

---

## 2. 推理蒸馏模型：本地分类场景的陷阱

### 问题 2.1：`<think>` 块无法抑制

推理蒸馏模型（Qwopus、DeepSeek-R1）的 `<think>` 块不是 Modelfile 模板层，而是**权重级别固化**的推理路径。无论怎么设 prompt、temperature、`disable_thinking`，模型都会先输出 think 块再回答。

### 问题 2.2：think 长度与 prompt 非线性相关

| prompt 规模 | think 块 | 总耗时 | 质量 |
|------------|---------|--------|------|
| 极简（1行规则） | 短 | 4s | 差（说项目源码可以删） |
| 中等（4条规则） | 爆炸 | 60s 超时 | 未知（没输出完） |

不是"规则多 → think 多"，而是**规则一多 think 就失控**。模型在 think 里逐条分析、自我怀疑、反复确认，token 消耗指数增长。

### 问题 2.3：GGUF 模型的 JSON Schema 约束解码极慢

Ollama 的 `format` 参数（grammar-based constrained decoding）在 GGUF 格式的模型上性能极差。本机测试中，带 schema 约束比纯文本慢 5-10x。

### 问题 2.4：`/api/chat` vs `/api/generate` 无速度差异

虽然 generate 端点少一层 messages 协议开销，但实际测试中单请求耗时完全相同。瓶颈在模型推理本身，不在 HTTP 协议。

### Takeaway

> 推理蒸馏模型的设计目标是一次性深思熟虑的问答，不是高吞吐分类。对于本地批量分类场景，**标准 instruct 模型 + 纯文本输出 + 客户端解析**才是正道。JSON Schema 约束解码在 GGUF 上不可用。

---

## 3. 日志系统：从 ad-hoc 到结构化

### 问题 3.1：`eprintln!` — 终端打印与进度行冲突

原始代码全部用 `eprintln!` 输出日志。但 enrichment 的 supervisor 需要 `\r` 实时刷新进度行（吞吐量/ETA），`eprintln!` 的换行符会把进度行顶出屏幕。

**解决**：迁移到 `flexi_logger` — 正常日志落盘 + 终端，`eprint!` 进度行不换行独立渲染。

### 问题 3.2：`env_logger` 不够 — 无法落盘

`env_logger` 只能输出到终端或一个文件流，不能同时写文件 + 终端（且保持进度行不被打乱）。

**解决**：`flexi_logger` 支持 `FileSpec` + `Duplicate` 模式。

### Takeaway

> 对于有实时进度条的 CLI 工具，早期就要规划好日志与进度的输出分离。`flexi_logger` 是 Rust 生态中最灵活的方案。

---

## 4. 测试：并发逻辑的单元测试是必需品

probe 逻辑（cwnd 决策 + SRTT 计算 + peak 跟踪）有 15+ 个单元测试覆盖：

- GROWTH/SHRINK/steady 每种决策
- cwnd 边界（最低、最高）
- SRTT EWMA 收敛
- inflight 永不负数
- 并发 acquire/release 无死锁

### Takeaway

> 并发控制逻辑必须拆成纯函数并提供 `test helper`（如 `ctl_with_srtt()` 注入任意 SRTT 值），让每种决策路径都可独立验证。不能依赖集成测试等 500ms 一个探针周期来覆盖所有分支。

---

## 5. 模型选型：benchmark 脚本一次跑完

模型选型不能靠"感觉"，必须用同一份数据、同样参数跑 benchmark。脚本 `scripts/bench_models.ps1` 自动化了全流程。

### Takeaway

> 准备一个可复现的 benchmark 脚本比手动逐个测试省 10 倍时间。benchmark 用 snapshot 文件保证确定性，避免扫描带来的 I/O 变化。

---

## 哪些事不该花时间

| 事情 | 浪费的时间 | 原因 |
|------|-----------|------|
| Qwopus 深度优化 | ~2h | 推理蒸馏模型的 `<think>` 是权重级固化的，不是 prompt 工程能解决的 |
| JSON Schema 约束解码 | ~30min | GGUF 的 grammar 解码有性能 bug，不如纯文本 + 客户端解析 |
| Opus-Distilled 调参 | 被 benchmark 自然淘汰 | 16x 更慢无质量提升，数据说话 |
| `/api/chat` vs `/api/generate` 纠结 | ~15min | 实测无差异，模型推理是瓶颈 |

> 核心原则：**用数据做决定，不要用感觉**。怀疑一个模型/方案 → 写 benchmark 跑数据 → 数据说话。
