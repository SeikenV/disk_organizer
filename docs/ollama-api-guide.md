# Ollama API 官方使用指南

> ⚠️ **已废弃（历史记录）**：本项目**已不再使用 Ollama**。子项目 A 将后端统一到自管理的
> llama.cpp `llama-server`（OpenAI 兼容 API），并移除了 `ollama-rs` 依赖。本文仅作早期实现的历史留存。
> **当前的推理后端与调用方式见 [`docs/ARCHITECTURE.md`](ARCHITECTURE.md) §2、§5**，
> 客户端实现见 `engine/src/enrich/client.rs`。
>
> ---
>
> 本文总结自 [Ollama 官方文档](https://docs.ollama.com)。（以下为历史内容。）

---

## 1. `/api/generate` 请求结构

所有 LLM 调用统一使用 `POST /api/generate`。本项目不使用流式输出（`stream: false`），适合批量分类场景。

### 1.1 完整参数表

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|------|--------|------|
| `model` | string | 是 | - | 模型名称，如 `"qwen3.5:0.8b"` |
| `prompt` | string | 否 | - | 用户提示词 |
| `system` | string | 否 | - | **系统提示词**（顶层参数，不应拼进 prompt） |
| `stream` | boolean | 否 | `true` | 是否流式输出。本项目设 `false` |
| `think` | boolean / `"low"` / `"medium"` / `"high"` | 否 | - | **顶级参数**，控制推理/思考阶段。本项目设 `false` |
| `format` | string 或 JSON schema object | 否 | - | `"json"` 字符串或 JSON Schema 对象，约束输出格式 |
| `raw` | boolean | 否 | `false` | 绕过模板，直接给模型原始 prompt |
| `images` | string[] | 否 | - | Base64 编码图片（多模态模型） |
| `keep_alive` | string / number | 否 | - | 模型在内存中保持时间，如 `"5m"` |
| `options` | object | 否 | - | 运行时生成参数（temperature、num_predict 等） |

### 1.2 本项目 Rust 结构体映射

```rust
#[derive(Serialize)]
struct GenerateRequest {
    model: String,
    prompt: String,
    stream: bool,           // false
    think: bool,            // false（顶级参数，非 options 内）
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<GenerateOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    format: Option<serde_json::Value>,
}

#[derive(Serialize)]
struct GenerateOptions {
    temperature: f64,       // 0.1（低温度获得稳定输出）
    num_predict: u32,       // 300（安全上限，think:false 下模型自然停止）
}
```

### 1.3 响应结构

```json
{
  "model": "qwen3.5:0.8b",
  "created_at": "2026-06-12T08:00:00Z",
  "response": "category: src directory; purpose: Keep project source code",
  "thinking": "",
  "done": true,
  "done_reason": "stop",
  "total_duration": 848000000,
  "load_duration": 1234567,
  "prompt_eval_count": 32,
  "prompt_eval_duration": 123456,
  "eval_count": 17,
  "eval_duration": 724000000
}
```

- `response`：最终回答文本
- `thinking`：推理过程（`think: false` 时为空）
- `done_reason`：`"stop"` 表示自然停止，`"length"` 表示被 `num_predict` 截断
- `eval_count`：实际生成的 token 数

---

## 2. Thinking（推理/思考）

### 2.1 核心概念

支持 thinking 的模型（Qwen3、DeepSeek-R1、GPT-OSS）会在回答前产生推理追踪。Ollama 将推理和回答**分离**：

- `thinking` 字段 → 推理过程
- `response` / `content` 字段 → 最终回答

### 2.2 控制方式

`think` 是 `/api/generate` 和 `/api/chat` 的**顶级参数**，不是 `options` 下的字段。

```json
// 关闭 thinking（推荐用于分类任务）
{"model": "qwen3", "prompt": "...", "think": false}

// 开启 thinking
{"model": "deepseek-r1", "prompt": "...", "think": true}
```

| 模型 | `think` 值 |
|------|-----------|
| Qwen3、DeepSeek | `true` / `false` |
| GPT-OSS | `"low"` / `"medium"` / `"high"`（不可关闭） |

### 2.3 默认行为

**注意**：thinking 默认是**开启**的。如果不加 `"think": false`，Qwen3 等模型会把所有 token 花在 thinking 上，response 始终为空。

### 2.4 本项目策略

```rust
// 分类任务：关闭 thinking，获取直接分类结果
think: false
```

分类任务不需要推理步骤，直接输出 `category: ... purpose: ...` 即可。关闭 thinking 后：
- Qwen3.5:0.8b 分类耗时 ~850ms
- 生成 ~17 tokens，自然停止（`done_reason: "stop"`）

---

## 3. Structured Outputs（结构化输出）

### 3.1 两种模式

| 模式 | `format` 值 | 说明 |
|------|------------|------|
| 自由 JSON | `"json"` | 要求模型输出 JSON，但不限制结构 |
| JSON Schema | JSON Schema 对象 | 严格约束输出的 JSON 结构 |

### 3.2 JSON Schema 示例

```json
{
  "type": "object",
  "properties": {
    "name": {"type": "string"},
    "capital": {"type": "string"},
    "languages": {"type": "array", "items": {"type": "string"}}
  },
  "required": ["name", "capital", "languages"]
}
```

### 3.3 注意

- 结构化输出配合 `temperature: 0` 获得更确定性的结果
- HuggingFace/ModelScope 的 GGUF 模型可能对 constrained decoding 性能不佳
- 本项目报告生成使用 JSON Schema，简单分类使用纯文本 + 客户端解析

### 3.4 本项目使用

```rust
// 报告生成：使用 JSON Schema 约束输出
format: Some(serde_json::json!({
    "type": "object",
    "properties": {
        "overview": {"type": "string"},
        "safe_summary": {"type": "string"},
        "caution_advice": {"type": "string"},
        "cleanup_plan": {"type": "array", "items": {"type": "string"}}
    },
    "required": ["overview", "safe_summary", "caution_advice", "cleanup_plan"]
}))

// 简单分类：纯文本 + parse_summary() 客户端解析
format: None  // 不传 format，让模型自由输出
```

---

## 4. Streaming（流式输出）

### 4.1 核心概念

- REST API 默认 `stream: true`
- Python/JS SDK 默认 `stream: false`（显式设 `stream=True`）
- 每个 chunk 是增量 JSON 对象

### 4.2 流式 chunk 结构

```python
for chunk in stream:
    chunk.message.thinking   # 推理增量（think: true 时）
    chunk.message.content    # 回答增量
    chunk.message.tool_calls # 工具调用增量
```

### 4.3 本项目策略

```rust
stream: false  // 批量分类不需要流式，直接等完整响应
```

批量目录分类场景下，流式输出无优势（我们不需要实时 UI 渲染），`stream: false` 简化实现。

---

## 5. Temperature 与 num_predict

### 5.1 推荐设置

| 场景 | temperature | num_predict | 说明 |
|------|-------------|-------------|------|
| 分类/提取 | 0.0 ~ 0.1 | 300 | 低温度获得稳定输出 |
| 创意写作 | 0.7 ~ 1.0 | 1024+ | 高温度增加多样性 |
| 代码生成 | 0.2 | 512+ | 需要精确但略有变化 |

### 5.2 本项目设置

```rust
options: Some(GenerateOptions {
    temperature: 0.1,    // 低温度，分类结果稳定
    num_predict: 300,     // 安全上限（think:false 下 ~20 tokens 自然停止）
})
```

---

## 6. 官方 Python / JavaScript 示例

### 6.1 Python (ollama 库)

```python
from ollama import chat

# 基本调用
response = chat(
    model='qwen3',
    messages=[{'role': 'user', 'content': 'Hello'}],
    think=False,      # 顶级参数
    stream=False,
)
print(response.message.content)

# 结构化输出
from pydantic import BaseModel

class Country(BaseModel):
    name: str
    capital: str
    languages: list[str]

response = chat(
    model='gpt-oss',
    messages=[{'role': 'user', 'content': 'Tell me about Canada'}],
    format=Country.model_json_schema(),
)
country = Country.model_validate_json(response.message.content)
```

### 6.2 JavaScript (ollama 库)

```javascript
import ollama from 'ollama'

// 基本调用
const response = await ollama.chat({
  model: 'qwen3',
  messages: [{ role: 'user', content: 'Hello' }],
  think: false,
  stream: false,
})
console.log(response.message.content)
```

### 6.3 cURL（本项目使用方式）

```bash
curl http://localhost:11434/api/generate -d '{
  "model": "qwen3.5:0.8b",
  "prompt": "System: ...\n\nUser: ...\n\nReply:",
  "stream": false,
  "think": false,
  "options": {"temperature": 0.1, "num_predict": 300}
}'
```

---

## 7. 本项目最佳实践总结

### 7.1 必须遵守

1. **`think` 是顶级参数**：放在请求根级，不是 `options` 内。设 `false` 禁用推理。
2. **`stream: false`**：批量场景不需要流式。
3. **`temperature: 0.1`**：分类任务需要稳定输出。
4. **`num_predict` 设安全上限**：即使 `think: false` 模型自然停止，也设一个合理的上限防止异常。

### 7.2 可选优化（待实施）

- **使用 `system` 参数**：当前把 system prompt 拼接到 prompt 字符串中（`"System: ...\n\nUser: ...\n\nReply:"`），可以改为使用 `/api/generate` 的 `system` 顶层参数，语义更清晰。
- **更广泛使用 `format` (JSON Schema)**：当前只有 report 使用 JSON Schema，简单分类也可以考虑使用。

### 7.3 已废弃的做法

- ❌ `options.enable_thinking` — 不是官方参数，无效
- ❌ `strip_think()` 解析 `<think>...</think>` 文本 — `think: false` 后模型不再产出 thinking blocks
- ❌ 通过限制 `num_predict` 来「等待」thinking 结束 — 不可靠且浪费

---

## 8. 参考链接

- [Ollama API Introduction](https://docs.ollama.com/api/introduction)
- [Generate API](https://docs.ollama.com/api/generate)
- [Streaming](https://docs.ollama.com/capabilities/streaming)
- [Thinking](https://docs.ollama.com/capabilities/thinking)
- [Structured Outputs](https://docs.ollama.com/capabilities/structured-outputs)
- [GitHub Libraries](https://github.com/ollama/ollama?tab=readme-ov-file#libraries-1)

---

*最后更新：2026-06-12*
