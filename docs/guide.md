# Kubach — руководство

Подробности о сборке, хранилище kubeconfig и MCP. Обзор проекта — в [README](../README_RU.md).

Кроссплатформенная (macOS, Windows, Linux) Kubernetes IDE на Rust и
[GPUI](https://gpui.rs) ([GPUI Kit](https://gpui-kit.com)).

Kubach работает локально, с правами из вашего kubeconfig: в кластер ничего не
устанавливается ([ADR 0004](adr/0004-local-only.md)). Ещё он умеет быть MCP-сервером,
чтобы ИИ-агент (Claude Code, Claude Desktop) работал с кластером в заданных вами рамках.

## Сборка и запуск

Нужен Rust (edition 2024, свежий stable) и Git.

```sh
cargo run --release                        # приложение
cargo run -p kubach-ui --example gallery   # галерея дизайн-системы
```

Бинарь называется `kubach`. Для разработки хватает `cargo run`: dev-профиль уже
оптимизирует зависимости.

Иконка приложения лежит в [`crates/kubach-app/assets/icon`](../crates/kubach-app/assets/icon):
исходник `kubach.svg` и собранные из него (кубы на скруглённой тёмной подложке по сетке иконок Apple) `kubach.icns`, `kubach.ico` и `kubach-512.png` (она же — Dock при `cargo run`).
После правки SVG их пересобирает `cargo run -p kubach-app --example icons`.

- **macOS** — при `cargo run` иконку в Dock ставит само приложение. Подписанный
  `Kubach.app` собирает `scripts/bundle-macos.sh --profile <файл.provisionprofile>`
  (`--debug` — быстрая dev-сборка). Bundle ID и команда берутся из профиля, сертификат —
  из Keychain. Подпись с entitlement `keychain-access-groups` нужна для разблокировки
  хранилища по Touch ID ([ADR 0008](adr/0008-kubeconfig-vault.md)): при `cargo run`
  она недоступна. Нужен профиль macOS App Development (этот Mac в списке устройств) с
  сертификатом Apple Development или Developer ID с сертификатом Developer ID Application.
- **Windows** — иконка вшивается в `kubach.exe` при сборке.
- **Linux** — `dev.kubach.app.desktop` копируется в `~/.local/share/applications`,
  а `kubach-512.png` — в `~/.local/share/icons/hicolor/512x512/apps/dev.kubach.app.png`.

Системные зависимости определяет GPUI, в репозитории они не перечислены. Смотрите
требования GPUI для вашей ОС:

- **macOS** — Xcode (полный, не только Command Line Tools; нужен компилятор Metal).
- **Windows** — Visual Studio Build Tools с C++ и Windows SDK.
- **Linux** — компилятор C/C++, `pkg-config` и dev-пакеты для X11/Wayland, xkbcommon,
  Vulkan и fontconfig (см. ниже).

### Сборки и релизы

[`.github/workflows/build.yml`](../.github/workflows/build.yml) на каждый коммит в `main`
собирает пакеты для x64 и arm64 и кладёт их в артефакты run:

- **Linux** (Ubuntu 22.04, glibc 2.35) — `.deb`, `.rpm`, Arch `.pkg.tar.zst`, AppImage и
  `tar.gz` ([`scripts/package-linux.sh`](../scripts/package-linux.sh), nfpm и appimagetool).
- **macOS** — `Kubach.app` в `.dmg` (`scripts/bundle-macos.sh --dmg`).
- **Windows** — portable `zip` и MSI ([`scripts/package-windows.ps1`](../scripts/package-windows.ps1), WiX 5).

Релиз — это коммит в `main`, поднимающий версию в `Cargo.toml`: если тега с этой версией
(`2026.10.2`) ещё нет, run после сборки сам ставит тег и выкладывает те же файлы в
GitHub Release вместе с `SHA256SUMS` и PKGBUILD для AUR (`kubach-bin`). macOS-сборка подписывается и
нотаризуется, только если заданы секреты (список — в начале workflow). Без них у
приложения ad-hoc подпись: Touch ID недоступен, хранилище открывается паролем.

### Linux

Проверено на Debian 12 (bookworm) с актуальным stable Rust: `cargo build`,
`cargo clippy --workspace --all-targets -- -D warnings` и `cargo test --workspace`
проходят. Пакеты для Debian/Ubuntu (тот же список — в
[`.github/actions/linux-deps`](../.github/actions/linux-deps/action.yml)):

```sh
sudo apt-get install -y build-essential pkg-config \
  libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev \
  libx11-dev libx11-xcb-dev libxcb1-dev \
  libfontconfig-dev libfreetype-dev libvulkan-dev \
  libssl-dev libzstd-dev
```

`libdbus` не нужен: D-Bus (logind, Secret Service) работает через чистый Rust `zbus`.
Для запуска нужен Vulkan-драйвер (на Debian/Ubuntu — `mesa-vulkan-drivers`).

Что зависит от окружения:

- **Настройки и хранилище** — в `$XDG_CONFIG_HOME/kubach` (по умолчанию `~/.config/kubach`)
  или в `KUBACH_CONFIG_DIR`.
- **Keychain** — Secret Service по D-Bus (GNOME Keyring, KWallet). Нужен только для режима
  «Keychain» у хранилища kubeconfig; если службы нет, режим недоступен с указанием
  причины, а пароль работает.
- **Блокировка по экрану** — свойство `LockedHint` сессии logind. Его выставляют
  GNOME, KDE и другие экраны блокировки, интегрированные с logind; без logind
  (контейнер, нет GUI-сессии) хранилище блокируется только вручную и по таймеру.
- **Окружение shell** — при запуске из `.desktop`-файла (без терминала) `PATH` и
  `KUBECONFIG` берутся из `$SHELL -l -i`, как на macOS.

## Подключение кластеров

Kubach читает `KUBECONFIG` (или `~/.kube/config`). Дополнительно кластеры добавляются
кнопкой «+» на панели кластеров («Add cluster») или в Settings → Clusters:

- **Link kubeconfig file** (по ссылке) — файл читается на месте и подмешивается после
  `KUBECONFIG`. Изменения от `aws`, `gcloud` и других утилит подхватываются, учётные
  данные остаются в вашем файле.
- **Import kubeconfig file** / **Paste kubeconfig from clipboard** — конфиг копируется в
  зашифрованное хранилище `vault.kubach` в папке настроек (`KUBACH_CONFIG_DIR` или
  папка ОС). Подходит для файлов, которые негде хранить: от коллеги или из облачной
  консоли. Шифрование — XChaCha20-Poly1305, ключ из пароля (Argon2id) и/или в системном
  хранилище ключей ([ADR 0008](adr/0008-kubeconfig-vault.md)). При импорте
  показывается предпросмотр контекстов; конфликтующие имена контекстов переименовываются.
- **Блокировка хранилища** — расшифрованные конфиги живут только в памяти. Хранилище
  блокируется вручную («Lock imported clusters»), по таймеру бездействия (15 минут по
  умолчанию) и при блокировке экрана ОС; после этого сессии импортированных контекстов
  закрываются. Пароль спрашивается при первом подключении к такому контексту.
  Таймер и блокировка по экрану настраиваются в Settings → Clusters.

## MCP: ИИ-агенты

Агент не получает учётных данных: он видит только MCP-tools, а запросы к API делает Kubach
([ADR 0005](adr/0005-mcp-permissions.md)). Сессия ограничена контекстом, namespace и
профилем — потолком того, что агент может попросить:

| Профиль | Что разрешено |
|---------|---------------|
| `read-only` | чтение ресурсов namespace и events, без значений Secrets |
| `debug` | read-only, плюс логи и exec |
| `deploy` | read-only, плюс изменения (apply, rollout, scale, delete) |

Права выдаются по запросу (#227): сессия стартует без прав, и когда агенту впервые нужно
право — читать namespace, раскрыть конкретный Secret, менять namespace, — Kubach спрашивает в
своём окне. Ответ, «да» или «нет», действует до конца сессии; отказ повторно не спрашивается.
Выданные права видны и отзываются в Settings → AI agent.

Доступ выключен по умолчанию. Включите его для нужного контекста в Settings → Security
(«AI agent access»): там же максимальный профиль, TTL сессии (до 8 ч), число namespace,
режим изменений («Dry run and diff only» или «Apply after confirmation»), exec и
«Secrets for the agent»: ничего, метаданные и проверки без значений (по умолчанию) или ещё
раскрытие по запросу — на каждый Secret отдельный вопрос с предупреждением, что значение уйдёт
провайдеру LLM. Если значения Secrets скрыты от вас самих, раскрытие агенту недоступно.

### Топология для агента

Два read-инструмента (#327, [ADR 0016](adr/0016-topology.md)) — права как у остальных
чтений: только namespace сессии, право чтения каждого, запись в аудит.

- `get_topology` — граф связей компактным JSON: узлы (id, kind, namespace, name, presence,
  поды, порты Service, хосты, изоляция NetworkPolicy), рёбра (маршруты, endpoints, ссылки на
  ConfigMap/Secret — только имена, вердикт политики) и диагностики с сообщениями. Параметры:
  `namespace` или `namespaces` (до 3), `resource` `{kind, name, namespace?}` с `depth` —
  окрестность ресурса, `detail` (`workloads`/`pods`), `networkPolicies` (по умолчанию `true`).
- `can_reach` — `from`, `to` (`{kind, name, namespace?}` workload/pod, `to` может быть
  Service, или `{address}` — IP вне кластера), `port` (номер или имя), `protocol` → разрешено
  ли соединение и почему: политики egress источника и ingress цели. Это анализ объектов
  NetworkPolicy; применяет ли их CNI — неизвестно.

Данные — одноразовый list нужных видов клиентом сессии; виды, которые политика или API не
дали, перечислены в `unavailable`, их узлы — `unknown`.

### Два способа подключения

- **stdio** — `kubach mcp`, без окна; агент запускает его как дочерний процесс:

  ```text
  kubach mcp [--context NAME] [--namespace NS]... [--profile PROFILE] [--ttl DURATION] [--grant-read]
  ```

  `-c/--context` (по умолчанию стартовый контекст из настроек, иначе `current-context`),
  `-n/--namespace` (можно повторять, если контекст позволяет), `-p/--profile`
  (`read-only`, `debug`, `deploy`; потолок сессии, по умолчанию максимум контекста), `--ttl` (`30m`, `2h`,
  `1h30m` или минуты), `--grant-read` (чтение namespace сессии разрешено сразу, без вопроса в
  окне, — чтения работают и без запущенного приложения; Secrets и изменения всё равно
  спрашиваются, настройки контекста не обходятся). Подробнее — `kubach mcp --help`.
- **HTTP** — Settings → AI agent → «Serve MCP over HTTP». Сервер слушает
  `http://127.0.0.1:<порт>/mcp` (порт фиксированный), пока запущен Kubach, и требует
  bearer-токен. Токен хранится в папке настроек, читается только вами; его можно
  перевыпустить, старые конфиги агентов перестанут работать.

### Сниппеты

Готовые команды с подставленными путём, контекстом и токеном копируются из
Settings → AI agent → «Connect an agent». Вид такой:

```sh
# Claude Code, HTTP: работает в сессии, запущенной в приложении
claude mcp add --transport http kubach http://127.0.0.1:<порт>/mcp \
  --header "Authorization: Bearer <токен>"

# Claude Code, stdio
claude mcp add kubach -- /путь/к/kubach mcp \
  --context <context> --namespace <namespace>
```

Claude Desktop — в `claude_desktop_config.json` (Settings → Developer → Edit Config):

```json
{
  "mcpServers": {
    "kubach": {
      "command": "/путь/к/kubach",
      "args": ["mcp", "--context", "<context>", "--namespace", "<namespace>"]
    }
  }
}
```

### Подтверждения и аудит

Вопросы о правах и изменения (apply, delete, exec — каждый вызов; scale и rollout restart —
один раз на namespace, если граница держится и на API-сервере) подтверждаются в окне Kubach.
Для `kubach mcp` это значит, что приложение должно быть запущено с включённым HTTP-сервером,
иначе вызовы, которым нужен ответ пользователя, отклоняются. Активные сессии с их правами и
журнал аудита (без значений Secrets) видны в Settings → AI agent.

## Документация

- [Архитектура](architecture.md)
- [ADR](adr/README.md): [0004 — только локально](adr/0004-local-only.md),
  [0005 — права MCP](adr/0005-mcp-permissions.md), [0006 — Helm](adr/0006-helm.md),
  [0007 — Kustomize](adr/0007-kustomize.md),
  [0008 — хранилище kubeconfig](adr/0008-kubeconfig-vault.md)
- [Правила проекта](../AGENT.md)

Для Helm-действий (rollback, upgrade, uninstall) нужен установленный `helm`, для сборки
Kustomize — `kustomize` или `kubectl`; просмотр работает и без них.

## Структура репозитория

- `crates/kubach-core` — доменная логика Kubernetes, без UI-зависимостей
- `crates/kubach-mcp` — MCP-сервер: политики, сессии, аудит
- `crates/kubach-ui` — дизайн-система: темы Kubach Dark / Light (`themes/kubach.json`), компоненты
- `crates/kubach-app` — приложение (бинарь `kubach`)

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Версионирование

`YEAR.MONTH.INC`, например `2026.10.1`. Месяц без ведущего нуля (требование SemVer в
Cargo), `INC` сбрасывается каждый месяц. Версия живёт в корневом `Cargo.toml`.
