---
type: opportunity
status: active
title: rust-code
created: 2026-03-05
tags: [rust, agent, cli, baml, sgr, ai, tui, ratatui, nucleo]
---

# PRD: rust-code

## 1. Problem
Текущие coding-агенты (Claude Code, Aider, OpenCode) написаны на Python или TypeScript. Это тянет за собой проблемы с дистрибуцией (зависимости от Node/uv/pip), высокое потребление памяти и хрупкость вызова инструментов (JSON parsing errors). Также им часто не хватает по-настоящему быстрого, интерактивного интерфейса со встроенным поиском.

## 2. Solution
`rust-code` — единый бинарный TUI/CLI-инструмент на Rust для автономной работы с кодовой базой и парного программирования в терминале.

Ключевые отличия:
1. **Schema-Guided Reasoning (SGR) через BAML**: Абсолютная строгая типизация рассуждений агента и вызова инструментов. Модель не может вернуть "битый" JSON — BAML гарантирует стриминг в Rust-структуры (enums) с автоматическими ретраями.
2. **Terminal UI (TUI) как в Neovim**: Построен на `ratatui` + `crossterm`. Позволяет иметь сплит-панели (чат, план агента, статус тулов).
3. **Fuzzy Search**: Встроенный движок `nucleo` (из Helix/Television) для мгновенного поиска файлов и контента, доступный как пользователю, так и агенту.
4. **Бесшовная интеграция с $EDITOR**: Возможность по клику/команде проваливаться из TUI прямо в Neovim для ручных правок, с автоматическим возвратом в сессию.

## 3. Core Features
- **Strict SGR Execution Loop**: "Анализ -> Планирование -> Выбор инструмента (Routing) -> Исполнение". Полностью типизировано через BAML.
- **Async TUI**: Главный поток рендерит UI (60 FPS) и обрабатывает ввод, фоновый поток (Tokio) гоняет SGR-цикл с LLM, общаясь через `mpsc` каналы.
- **LLM Providers**: Встроенная поддержка `gemini-3.1-flash-lite-preview` (Vertex/Google) и `claude-3.5-sonnet` (OpenRouter) с конфигурацией BAML (портировано из `va-agent`).
- **Fast Tools**: 
  - Нативные файловые операции (`std::fs`).
  - Быстрый поиск и навигация (встроенный `nucleo`).
  - Интеграция с Git (через `git2-rs` или bash).
  - Shell-команды.
- **Zero-Dependency Binary**: Установил один бинарник и работаешь.

## 4. Architecture (Cargo Workspace)

Проект разбит на изолированные крэйты:

- `rc-cli` (Entrypoint, UI): 
  - `clap` для аргументов запуска.
  - `ratatui` + `crossterm` для отрисовки TUI.
  - `tui-textarea` для многострочного ввода промптов (vim-keys).
  - Интеграция с `nucleo` для всплывающих окон поиска.
- `rc-core` (Orchestration): 
  - Главный execution loop (SGR).
  - Управление контекстом, историей диалога и стейтом.
  - Токены каналов (`mpsc`) для связи с UI.
- `rc-tools` (Capabilities): 
  - Изолированные функции-инструменты (`read_file`, `write_file`, `bash`, `fuzzy_find`, `open_editor`).
- `rc-baml` (LLM Interface): 
  - `.baml` схемы (SGR структуры, Enums для тулов).
  - Клиенты Vertex AI, Gemini, Local, сгенерированные Rust-типы.

## 5. Timeline / MVP Scope

- **Phase 1: Workspace & BAML Foundation**
  - Поднять Cargo workspace.
  - Настроить `rc-baml` (перенести провайдеров из `va-agent`), описать базовую схему `NextStep` (Action Enum).
- **Phase 2: Basic Tools & Core Loop**
  - Реализовать `rc-tools` (fs, bash).
  - Написать SGR диспетчер в `rc-core` (match по BAML enum).
- **Phase 3: TUI Foundation**
  - Создать базовый UI в `rc-cli` (панель чата, панель ввода `ratatui-textarea`).
  - Связать фоновый поток агента с UI через каналы (отображение прогресса/тулов).
- **Phase 4: Advanced TUI & Nucleo**
  - Встроить `nucleo` для fuzzy поиска (отдельный popup).
  - Добавить тул `OpenInEditor` (сворачивание TUI -> открытие Neovim -> разворачивание TUI).

## 6. SGR Scheme Concept (BAML)
```baml
class NextStep {
  analysis string @description("Понимание текущей ситуации, что уже сделано")
  plan_updates string[] @description("План дальнейших действий (кратко)")
  action ToolAction @description("Следующее действие")
}

union ToolAction {
  ReadFile
  WriteFile
  BashCommand
  SearchCode
  AskUser
  FinishTask
}
```
