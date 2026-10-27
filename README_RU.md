<p align="center">
  <img src="crates/kubach-app/assets/icon/kubach-512.png" width="128" alt="Kubach">
</p>

<h1 align="center">Kubach</h1>

<p align="center">
  <b>Нативная Kubernetes IDE с GPU-ускорением — и безопасная дверь для вашего ИИ-агента.</b>
</p>

<p align="center">
  <a href="https://github.com/BigTows/kubach/releases"><img src="https://img.shields.io/github/v/release/BigTows/kubach?label=release" alt="Release"></a>
  <img src="https://img.shields.io/badge/macOS%20%C2%B7%20Windows%20%C2%B7%20Linux-x64%20%26%20arm64-informational" alt="Platforms">
  <img src="https://img.shields.io/badge/built%20with-Rust%20%2B%20GPUI-orange" alt="Rust + GPUI">
  <img src="https://img.shields.io/badge/vibe-coded%20%F0%9F%A4%96-blueviolet" alt="Vibe-coded">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-green" alt="MIT"></a>
</p>

<p align="center">
  <a href="README.md">English</a> · <b>Русский</b> · <a href="README_CN.md">简体中文</a>
</p>

---

> [!NOTE]
> **Kubach — vibe-coded проект.** Почти весь код написали ИИ-агенты, а человек задаёт
> направление, проверяет результат и принимает решения. Говорим об этом сразу, чтобы
> вы знали, что запускаете.

## Почему Kubach

- **Нативный и быстрый.** Написан на Rust и [GPUI](https://gpui.rs) — GPU-фреймворке
  интерфейса, на котором сделан редактор Zed. Внутри нет ни Electron, ни браузера.
- **В кластер ничего не ставится.** Kubach работает на вашем компьютере с правами из
  вашего kubeconfig: никаких агентов в кластере, операторов и CRD.
- **Сделан для работы с ИИ.** Kubach — ещё и MCP-сервер. Claude Code, Claude Desktop или
  любой другой MCP-клиент работает с кластером в заданных вами рамках. Учётных данных
  агент не видит.
- **Kubeconfig под защитой.** Импортированные конфиги хранятся в зашифрованном хранилище:
  оно открывается паролем, **Touch ID** или **Windows Hello** и блокируется вместе с экраном.

## Возможности

**Изучайте**
- Несколько кластеров одновременно: контексты переключаются мгновенно, подключения не
  остывают.
- Все виды ресурсов, включая CRD, с живыми обновлениями (list + watch), событиями и
  диагностикой.
- **Топология**: граф от Ingress / Gateway API к Service, workload'ам и подам и дальше
  к ConfigMap, Secret и PVC. Подсвечивает битые ссылки и проверяет NetworkPolicy
  («достучится ли A до B по порту 443?»).
- Слой трафика из Prometheus для Istio, Linkerd и Cilium Hubble.
- CPU и память из metrics-server, история — из Prometheus.

**Управляйте**
- Логи, exec, port-forward, debug-контейнеры и shell на ноде.
- Scale, rollout restart, cordon / drain и eviction.
- YAML-редактор с подсветкой синтаксиса, diff перед применением и server-side apply.
- Secrets остаются секретами: значение запрашивается только по кнопке «Показать», не
  кэшируется и очищается из буфера обмена по таймеру.
- Встроенный терминал с `KUBECONFIG` только для текущего контекста.

**Выкатывайте**
- **Helm**: релизы, история, upgrade, rollback, репозитории чартов с иконками и README,
  OCI-чарты. Kubach может сам скачать закреплённую версию `helm` с проверкой контрольной
  суммы.
- **Kustomize**: сборка и diff оверлеев до применения.
- **GitOps**: показывает, какой Argo CD Application или Flux Kustomization / HelmRelease
  управляет ресурсом.

**Работайте вместе с ИИ** ([MCP](docs/guide.md#mcp-ии-агенты))
- Сессия ограничена контекстом, namespace и профилем: `read-only`, `debug` или `deploy`.
- Права выдаются по запросу: агент просит, а вы отвечаете «да» или «нет» в окне Kubach.
- Изменения, exec и раскрытие каждого Secret требуют вашего подтверждения или
  ограничиваются dry run и diff.
- Каждый вызов попадает в журнал аудита; значений Secrets в нём нет.
- Инструменты топологии для агента: `get_topology` и `can_reach`.
- Подключение по stdio (`kubach mcp`) или по HTTP на `127.0.0.1` с bearer-токеном.

**Как дома**
- Темы Kubach Dark и Kubach Light.
- Интерфейс на английском, русском и упрощённом китайском.

## Установка

Скачайте последнюю сборку со страницы **[Releases](https://github.com/BigTows/kubach/releases)**.
Каждая сборка есть для x64 и arm64.

| ОС | Пакет | Примечания |
|---|---|---|
| **macOS** 11+ | `Kubach-<version>-macos-<arch>.dmg` | Подписан Developer ID и нотаризован |
| **Windows** 10+ | установщик `.msi` или portable `.zip` | Пока без подписи кода: SmartScreen может предупредить |
| **Debian / Ubuntu** | `.deb` | `sudo apt install ./kubach_*.deb` |
| **Fedora / RHEL / openSUSE** | `.rpm` | `sudo dnf install ./kubach-*.rpm` |
| **Arch Linux** | `.pkg.tar.zst` или `PKGBUILD` (`kubach-bin`) | `sudo pacman -U kubach-*.pkg.tar.zst` |
| **Любой Linux** | `.AppImage` или `.tar.gz` | Нужны glibc 2.35+ и драйвер Vulkan |

Свежие сборки с каждого коммита в `main` лежат в
[артефактах Actions](https://github.com/BigTows/kubach/actions/workflows/build.yml).

### Подключите ИИ-агента

Сначала включите доступ агента для контекста в **Настройки → Безопасность**. По умолчанию
он выключен.

Kubach умеет сам прописаться в конфиг агента. Он покажет файл и изменение, применит их
после подтверждения, сохранит остальные серверы и сделает резервную копию файла:

```sh
kubach mcp install claude-code      # а также: codex, opencode, cursor, claude-desktop
kubach mcp clients                  # какие агенты подключены
kubach mcp uninstall cursor
```

Можно запускать stdio-сервер самому и настраивать сессию:

```sh
kubach mcp                                   # агент выбирает контекст, Kubach сначала спрашивает вас
kubach mcp --context prod --namespace shop   # закрепить контекст и namespace (-n можно повторять)
kubach mcp -c staging -p read-only --ttl 2h  # ограничить профиль и длительность сессии
kubach mcp -c dev -n web --grant-read        # разрешить чтение сразу: оно работает и без приложения
kubach mcp --help
```

| Параметр | Значение |
|---|---|
| `-c, --context NAME` | контекст kubeconfig (без параметров его выбирает агент, а вы подтверждаете) |
| `-n, --namespace NS` | ограничить сессию namespace; повторите для нескольких |
| `-p, --profile` | `read-only`, `debug` или `deploy`: максимум, который может запросить сессия |
| `--ttl` | длительность сессии: `30m`, `2h`, `1h30m` (по умолчанию 30 минут) |
| `--grant-read` | разрешить чтение namespace сессии без вопроса; Secrets и изменения всё равно подтверждаются |

Запросы прав и изменения подтверждаются в окне Kubach, поэтому держите приложение
запущенным с включённым **Настройки → ИИ-агент → HTTP-сервер**. Этот же HTTP-сервер может
обслуживать агентов напрямую по адресу `http://127.0.0.1:<port>/mcp` с bearer-токеном.
Готовые команды с вашим путём, портом и токеном — в **Настройки → ИИ-агент → Подключить агента**.

Где лежит бинарь: `/Applications/Kubach.app/Contents/MacOS/kubach` на macOS,
`/usr/bin/kubach` в Linux-пакетах, `C:\Program Files\Kubach\kubach.exe` на Windows.

## Сборка из исходников

Нужен свежий stable Rust и то, что требует GPUI на вашей платформе: Xcode на macOS,
Visual Studio C++ Build Tools на Windows, dev-пакеты X11/Wayland, xkbcommon, Vulkan и
fontconfig на Linux.

```sh
cargo run --release                        # приложение
cargo run -p kubach-ui --example gallery   # галерея дизайн-системы
```

В [руководстве](docs/guide.md) — подробности по платформам, хранилище kubeconfig,
настройка MCP и упаковка.

## Подробнее

- [Руководство](docs/guide.md): сборка, хранилище kubeconfig, настройка MCP.
- [Архитектура](docs/architecture.md) и [архитектурные решения (ADR)](docs/adr/README.md).
- [Правила проекта](AGENT.md): соглашения, которым следуют агенты.

Версии — `YEAR.MONTH.INC`, например `2026.10.1`.

## Лицензия

[MIT](LICENSE)
