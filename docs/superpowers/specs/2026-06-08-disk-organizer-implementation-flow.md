# disk_organizer — 实现流程文档（Implementation Flow）

> ⚠️ **已被取代（历史记录）**：本文是 2026-06-08 的开发前设计稿，多处与现状不符
> （jwalk→MFT；Ollama 串行→自管理 llama.cpp 多槽位；§8 的 ollama/qwen2.5 配置块已被 CLI 参数取代；
> §9 目录结构已变为 `engine/` workspace）。**当前权威流程见 [`docs/ARCHITECTURE.md`](../../ARCHITECTURE.md) §4**。
> 本文仅作历史留存。
>
> 版本：2026-06-08 · 状态：设计稿待评审 · 配套：功能文档、架构文档

## 1. 运行时端到端流程

```
启动
  │
  ├─[有快照?]──是──> 载入 Snapshot ──┐
  │                                  │
  └─否─> ① 并行扫描(jwalk)            │
            NTFS 计量(硬链接去重/跳重解析点)│
            └─> 构建 Node Tree ───────┤
                                       ▼
        ② 递归切割 + 规则分类(同步, 快)
            └─> 产出"初版清单"(规则标签) ──> 呈现层立即可见 ★秒级有结果
                                       │
        ③ 入队增强任务(后台)            │
            ├─ VERIFY  已知大目录体检    │  优先级: 高
            ├─ SUMMARIZE 未知大目录摘要  │  优先级: 中
            └─ (用户下钻才入队)逐文件     │  优先级: 中
                                       ▼
            文本 LLM 逐个完成 ──> 流式 ItemUpdate ──> 清单原地刷新
                                       │
        ④ 视频延后队列(最低优先级)       │
            "大 + 名字无意义"的视频      │
            └─ ffmpeg 抽帧 -> VLM 猜测 ─> 流式刷新
                                       ▼
        ⑤ 用户浏览: list / why N / open N(下钻触发逐文件)
        ⑥ 用户选择: select 12 14 19
        ⑦ 汇总将释放体积 + 风险分布 -> 二次确认
        ⑧ 删除(默认回收站) -> 删除报告
        ⑨ 更新并保存 Snapshot
```

## 2. 渐进增强状态机

每个 Item 的 `status` 单向推进；呈现层根据状态显示不同"成色"：

```
Pending ──规则命中──> RuleLabeled ──体检──> Verified
   │                      │
   │                      └──(未知目录)──> (LLM)LlmLabeled
   └──────────────────────────────────────> (视频)VisionLabeled
```

- `Pending/RuleLabeled` 即可展示（用户不必等 AI）。
- LLM/Vision 结果到达 → 发 `ItemUpdate` → 清单对应行就地更新（不重排，除非用户要求按置信度重排）。

## 3. 任务队列：优先级与并发

| 任务 | 优先级 | 并发 | 说明 |
|------|--------|------|------|
| 规则分类 | 同步 | 随扫描 | 不入队，切割时直接完成 |
| VERIFY 体检 | 高 | N(CPU) | 已知大目录优先确认"名实是否相符" |
| SUMMARIZE 摘要 | 中 | 受 Ollama 串行约束 | 未知大目录用途 |
| 逐文件 | 中(按需) | 同上 | 仅用户下钻的目录 |
| 视频 VLM | 低 | 1~2 | ffmpeg 抽帧 + VLM，最贵，最后做 |

> Ollama 单模型实例通常串行推理；队列负责"喂得有序"，文本阶段跑完再切视觉模型，避免来回换模型抖动。

## 4. 删除安全（零误删红线）

1. 默认**移入回收站**（Windows Shell API），不永久删除。
2. 删除前必有**二次确认**，并展示：条目数、总释放体积、各风险等级占比。
3. 风险为 `⛔系统关键` 的条目，选择时给**显著警告**，需额外确认。
4. 提供 `--dry-run`：只打印将删什么，不动手。
5. LLM/Vision 标注的条目**默认不预选**——AI 的话只作参考。

## 5. 错误处理

| 场景 | 行为 |
|------|------|
| 权限拒绝/文件被占用 | 跳过 + 计数，最后汇总"N 项无法访问" |
| Ollama 不可用 | 跳过所有增强，系统降级为"纯规则版"，照常可用 |
| ffmpeg 缺失 | 视频阶段禁用并提示，不影响其余 |
| 长路径/异常字符 | `\\?\` 前缀重试；仍失败则记录跳过 |

## 6. 实现里程碑（与功能文档对齐）

- **M1（地基可用）**：C1 扫描 + NTFS 计量 → C2 树 → C3 规则库(先少量手写) → C4 切割 → C9 CLI 清单+编号删除(回收站) → C8 快照。**交付：能诊断真实 C 盘、硬链接不虚高。**
- **M2（文本 AI）**：C5 + C7 队列 + 流式刷新；接 BleachBit CleanerML 扩充 C3；未知目录摘要 + 已知目录体检 + 按需逐文件。
- **M3（视觉）**：C6 + ffmpeg + VLM，视频延后队列。
- **M4（GUI）**：复选框 + treemap，复用 C1~C8 内核。

## 7. 测试策略

| 层 | 测试 | 方式 |
|----|------|------|
| 切割算法 C4 | 给定虚构树 → 断言切点与条目 | 单元测试（fixture 树） |
| 硬链接去重 C1 | 构造含硬链接的临时目录 → 断言只计一次 | 集成测试（临时 NTFS 目录） |
| 重解析点 C1 | 造一个 junction → 断言不跟随 | 集成测试 |
| Catalog C3 | 路径样例 → 断言匹配到正确条目 | 单元测试 |
| LLM 编排 C5 | mock Ollama 返回 → 断言解析/降级 | 单元测试（注入假 client） |
| 端到端 | 小型 fixture 盘 → 跑全流程 → 断言清单 | 集成测试 |

> AI 输出本身不做"正确性"断言（不稳定），只断言**调用契约与降级行为**；模型质量靠人工抽验。

## 8. 配置项（初版）

```
scan.threshold_bytes      = 100MB     # 进入清单的最小体量
scan.top_n                = 100
scan.follow_reparse       = false
llm.endpoint              = http://localhost:11434
llm.text_model            = qwen2.5:1.5b
llm.vision_model          = qwen2.5-vl:3b
llm.sample_files_per_dir  = 20        # 未知目录摘要抽样数
video.enable              = true
video.frames              = 4
delete.use_recycle_bin    = true
```

## 9. 目录结构（建议）

```
disk_organizer/
├─ src/
│  ├─ scanner/        # C1 + NTFS
│  ├─ tree/           # C2
│  ├─ catalog/        # C3 (+ cleanerml 解析)
│  ├─ classify/       # C4 切割引擎
│  ├─ enrich/         # C5/C6/C7 队列与编排
│  ├─ store/          # C8 快照
│  ├─ cli/            # C9
│  └─ main.rs
├─ catalog_data/      # 规则数据(YAML + 转换后的 CleanerML)
├─ tests/             # 集成测试 + fixtures
└─ docs/superpowers/specs/
```
