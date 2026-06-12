# Disk Organizer — 项目约定

## 技术栈
- Rust 2021 edition
- Windows-first（使用 NTFS MFT、Win32 API）
- LLM 调用：`ollama-rs` (v0.3.4, pepperoni21)
- JSON Schema 生成：`schemars` derive macro + `ollama_rs::generation::parameters::JsonStructure`

## 关键架构决策
- 双 GPU 后端支持（dGPU Ollama + 可选 iGPU llama-server）
- TCP 风格的拥塞控制（SRTT-based Cwnd 自适应）
- Selective Repeat 重传机制
- `think: false` 禁用推理阶段（Qwen3 模型会烧 token）
- 分类任务用结构化输出（`FormatType::StructuredJson`）

## 代码风格
- LLM 调用全部通过 `src/enrich/llm.rs`，不直接调 reqwest
- system prompt 用 `GenerationRequest.system()`，不拼接到 prompt
- 模型预热用 `preload_model()` 避免冷启动
- 测试全部在模块内 `#[cfg(test)] mod tests`
- 当前 87 个测试必须全部通过
