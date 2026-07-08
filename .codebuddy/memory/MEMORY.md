# Disk Organizer — 项目约定

## 技术栈
- Rust 2021 edition
- Windows-first（使用 NTFS MFT、Win32 API）
- LLM 推理：自管理 llama-server 子进程（已从 ollama-rs 切换到 llama.cpp）
- llama-server 通过 OpenAI 兼容 HTTP API 调用（/v1/chat/completions, /health）

## LLM 推理环境
- llama-server 二进制位于 `tools/llamacpp/<cpu|cuda|vulkan>/llama-server.exe`
- 模型文件位于 `tools/models/Qwen3.5-0.8B-UD-Q4_K_XL.gguf`（文本）、`SmolVLM2-500M-Video-Instruct-Q8_0.gguf`（视觉）、`mmproj-*.gguf`（投影器）
- 初始化脚本：`scripts/setup_llama_cpp.bat`（代替旧的 setup_ollama）
- 测试脚本：`scripts/test_llm.ps1`（含 pre-flight 检查）
- 引擎默认 `--tools-dir tools/llamacpp`, `--llm-model-path tools/models/Qwen3.5-0.8B-UD-Q4_K_XL.gguf`

## 关键架构决策
- 吞吐量驱动的拥塞控制
- 数据流通过 supervisor 路由（不直接调 reqwest）
- 模型预热用 `preload_model()` 避免冷启动
- 测试全部在模块内 `#[cfg(test)] mod tests`
