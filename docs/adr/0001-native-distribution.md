# Rust 单文件发版，Python 只作迁移基准

旧版虽已能打包成一个文件，仍内嵌 Python，且分散的流程难以维护。新版用一个按场景拆分的 Rust crate 发布 macOS Apple Silicon 和 Windows x64 可执行文件；保留功能与可沿用的机器输出，但选择更清楚的入口而非维持旧命令别名。以 Python 快照验收有意变化，也保留旧版供切换前使用；新版不自动替换旧版。
