# Настройки → ИИ-агент: HTTP-сервер MCP, подключение клиентов, активные
# сессии и журнал аудита.

settings-copy = Копировать
settings-copied = Скопировано в буфер обмена

## HTTP-сервер

settings-group-http-server = HTTP-сервер
settings-http-server-description = Нужен любому агенту: здесь Kubach спрашивает вас, когда агенту нужно право или изменение. HTTP-агенты ещё и подключаются к нему напрямую (127.0.0.1, с токеном ниже).
settings-server-off = Выключен.
settings-server-starting = Запуск…
settings-server-listening = Слушает { $url }
settings-server-not-running = Не запущен.
settings-mcp-http = MCP по HTTP
settings-mcp-port = Порт
settings-mcp-port-description = Постоянный, чтобы конфиги агентов работали после перезапуска.
settings-mcp-token = Токен
settings-mcp-token-description = { $token } · хранится в папке настроек, доступен для чтения только вам
settings-mcp-token-none = Создаётся при первом запуске сервера.
settings-mcp-token-regenerate = Создать заново
settings-mcp-token-regenerate-tooltip = Агенты со старым токеном перестанут работать

## Подключение агента

settings-group-connect = Подключить агента
settings-connect-description = «Подключить» записывает сервер `kubach` (это приложение, `kubach mcp`, без токена) в собственный конфиг агента после того, как вы увидите изменение. Затем агент работает с контекстом при запуске, называет namespace, и Kubach спрашивает вас здесь, когда агент впервые читает каждый.
settings-connect-unavailable = Kubach не может определить свой путь или вашу домашнюю папку; настройте вручную.
settings-manual-setup = Ручная настройка
settings-manual-setup-description = Для других клиентов или подключения по HTTP.
settings-manual-show = Показать
settings-manual-hide = Скрыть
settings-snippet-stdio = Любой клиент, stdio
settings-snippet-stdio-description = Большинство клиентов принимают это в списке MCP-серверов. Агент сам запускает `kubach mcp`; токен не нужен.
settings-snippet-claude-http = Claude Code по HTTP
settings-snippet-claude-http-description = Агент выбирает контекст через use_context, и Kubach спрашивает вас. Токен остаётся в своём файле: добавьте строку export в профиль оболочки, чтобы Claude Code его получил. Никогда не добавляйте его с --scope project: .mcp.json попадает в репозиторий.
settings-client-not-installed = не установлен
settings-client-not-connected = не подключён
settings-client-connected = подключён
settings-client-stale = подключён к другому kubach: { $command }
settings-client-broken = конфиг не читается: { $error }
settings-client-connect = Подключить
settings-client-reconnect = Переподключить
settings-client-disconnect = Отключить
settings-client-connect-title = Подключить { $client }?
settings-client-disconnect-title = Отключить { $client }?
settings-client-change-description = { $summary }. Файл принадлежит { $client }: другие серверы и настройки останутся, прежняя версия сохранится рядом как { $suffix }.
settings-client-apply = Применить
settings-client-connected-done = Подключено. { $hint }
settings-client-disconnected-done = Отключено. { $hint }
settings-plan-create = Создать { $path } с сервером `{ $server }`
settings-plan-add = Добавить сервер `{ $server }` в { $path }
settings-plan-update = Направить сервер `{ $server }` в { $path } сюда (раньше запускал { $command })
settings-plan-remove = Удалить сервер `{ $server }` из { $path }
settings-plan-nothing = { $path } менять не нужно
settings-restart-claude-code = Начните новую сессию Claude Code; /mcp покажет сервер.
settings-restart-codex = Начните новую сессию Codex; `codex mcp list` покажет сервер.
settings-restart-opencode = Перезапустите OpenCode.
settings-restart-cursor = Cursor подхватит его в Settings → MCP; если нет, перезагрузите окно.
settings-restart-claude-desktop = Полностью закройте Claude Desktop и запустите снова.

## Активные сессии

settings-group-sessions = Активные сессии
settings-sessions-description = Сессии HTTP и `kubach mcp` с правами, которые вы им дали. Отзыв сессии или права запрещает следующие вызовы; процесс `kubach mcp` узнаёт об этом за несколько секунд. Отозванное право в этой сессии больше не спрашивается.
settings-sessions-http-waiting = HTTP-агенты: контекст ещё не выбран. Агент выберет его через use_context, и Kubach спросит вас перед началом.
settings-sessions-none = Нет активных сессий.
settings-session-title = { $context } · до уровня «{ $profile }»
settings-session-no-calls = вызовов пока нет
settings-session-pid = kubach mcp, pid { $pid }
settings-session-vault = хранилище, закончится при блокировке
settings-session-namespace-not-read = { $namespace }: ещё не читался
settings-session-details =
    { $calls ->
        [one] { $origin } · { $clients } · { $calls } вызов · с { $since } · закончится через { $left }
        [few] { $origin } · { $clients } · { $calls } вызова · с { $since } · закончится через { $left }
       *[other] { $origin } · { $clients } · { $calls } вызовов · с { $since } · закончится через { $left }
    }
settings-session-no-namespace = Namespace пока нет: спросим, когда агент впервые прочитает какой-нибудь.
settings-session-raised = Потолок повышен для этой сессии: { $raised }
settings-session-no-grants = Права пока не выданы.
settings-boundary-client-only = ограничения только в Kubach
settings-boundary-impersonation = ограничения в Kubach и API-сервере (impersonation)
settings-boundary-temporary-sa = ограничения в Kubach и API-сервере (временный ServiceAccount)
settings-boundary = { $namespace }: { $who }
settings-boundary-warning = { $namespace }: { $who }. { $warning }
settings-grant-read = Чтение { $namespace }
settings-grant-secret = Secret { $namespace }/{ $name }
settings-grant-modify = Изменение { $namespace }
settings-grant-allowed-at-launch = разрешено через --grant-read
settings-grant-allowed = разрешено
settings-grant-denied = запрещено
settings-revoke = Отозвать
settings-revoking = Отзыв…

## Журнал аудита

settings-group-audit = Журнал аудита
settings-audit-description = Каждый вызов каждой сессии: инструмент, аргументы со скрытыми секретами, решение и результат. Единственная запись вызовов агента, когда граница только в Kubach.
settings-audit-recent = Последние вызовы
settings-audit-file = Сначала новые. Файл: { $path }
settings-audit-no-folder = В ОС нет папки настроек: ничего не записывается.
settings-audit-show-file = Показать файл
