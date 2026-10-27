# Kubach — правила проекта

Кроссплатформенная (macOS, Windows, Linux) Kubernetes IDE на Rust и GPUI.
Архитектура: [`docs/architecture.md`](docs/architecture.md), решения: [`docs/adr/`](docs/adr/).

## Структура крейтов

Rust workspace, edition 2024, крейты лежат в `crates/*`.

| Крейт | Назначение | Границы |
|-------|-----------|---------|
| `kubach-core` | Доменная логика k8s (будет на `kube-rs`) | **Без UI-зависимостей**: никакого GPUI и `gpui-kit` |
| `kubach-ui` | Дизайн-система: тема `themes/kubach.json` (Kubach Dark / Light), компоненты, галерея | Зависит от `gpui-kit` и `kubach-i18n` (встроенные тексты компонентов); не знает про k8s и `kubach-core` |
| `kubach-mcp` | MCP-сервер для ИИ-агентов (`rmcp`, `axum`): policy, сессии, идентичность, аудит ([ADR 0005](docs/adr/0005-mcp-permissions.md)) | Зависит только от `kubach-core`; без GPUI и `gpui-kit`; в API ходит своим клиентом сессии |
| `kubach-app` | Бинарь `kubach` на GPUI (`gpui-kit 0.8`) | Склеивает `core`, `ui` и `mcp`; здесь экраны и wiring |

Зависимости: `app → mcp → core`, `app → core`, `app → ui`; `core` и `ui` не зависят
друг от друга, `mcp` не зависит от `ui`.
Внешние зависимости объявляются в `[workspace.dependencies]` корневого `Cargo.toml`,
в крейтах — `dep.workspace = true`.

## Стиль

- Цвета только из темы: `cx.theme()`. Никаких хардкод-цветов в компонентах и экранах.
  Новые токены добавляются в `themes/kubach.json` (для Dark и Light).
- Новые общие компоненты — в `kubach-ui` (`src/components/`) и обязательно в галерею
  (`crates/kubach-ui/examples/gallery.rs`).
- Визуальный стиль — Vercel: полупрозрачные поверхности, сдержанная палитра.
- Переведённый текст длиннее английского (ru — до 1,5–2×): не ставить его в `w(px(..))`;
  колонка подписей — `kubach_ui::measure::label_column_width`, обрезка — `TruncatedText`
  (подсказка с полным текстом), а не голый `.truncate()` ([ADR 0018](docs/adr/0018-i18n.md)).
- Плагины ресурсов только встроенные (трейт `ResourcePlugin`), без динамической загрузки.
- Работа только локально, без агента в кластере.
- `cargo fmt` и `clippy` без предупреждений.

## Версионирование

`YEAR.MONTH.INC`, например `2026.10.1`. Месяц без ведущего нуля (требование SemVer в Cargo),
`INC` сбрасывается каждый месяц. Версия живёт в `[workspace.package]` корневого `Cargo.toml`.

## Команды

```sh
cargo build                                  # сборка workspace
cargo clippy --workspace --all-targets       # линтер
cargo fmt --all                              # форматирование
cargo run                                    # приложение
cargo run -p kubach-ui --example gallery     # галерея дизайн-системы
cargo test -p kubach-ui -p kubach-app --test a11y_ids --test a11y_check -- --ignored
                                             # дерево a11y галереи и окна без дублей id (macOS, откроет окна)
```

Dev-профиль оптимизирует зависимости (`opt-level = 3`), иначе GPUI тормозит.

## Pull request

- В описании PR обязательно `Closes #N` (номер задачи).
- Перед PR проходят `cargo fmt --all`, `cargo clippy`, `cargo build`.
- Изменение архитектурных решений сопровождается новым или обновлённым ADR в `docs/adr/`.
