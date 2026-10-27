# Статус-бар внизу главного окна. Имена Kubernetes (Pod, Namespace, watch)
# не переводятся и не склоняются.

status-loading-kubeconfig = Загрузка kubeconfig…
status-switch-context = Сменить контекст
status-error = Ошибка: { $error }

status-no-cluster = Нет кластера
status-cluster = { $context }
status-cluster-version = { $context } · { $version }
status-cluster-connecting = { $context } · подключение…
status-cluster-error = { $context } · ошибка

status-all-namespaces = Все namespace
status-no-namespace = Нет namespace
status-namespace = { $namespace }
status-namespaces = { $count } namespace
status-namespaces-tooltip = Показанные namespace
status-namespaces-tooltip-change = Показанные namespace · изменить (⌘⇧N / Ctrl+Shift+N)

status-syncing = Синхронизация…
status-reconnecting = Переподключение…
status-sync-loading =
    { $count ->
        [one] { $count } watch-поток загружается
        [few] { $count } watch-потока загружаются
        [many] { $count } watch-потоков загружаются
       *[other] { $count } watch-потока загружаются
    }
status-sync-reconnecting =
    { $count ->
        [one] { $count } watch-поток переподключается
        [few] { $count } watch-потока переподключаются
        [many] { $count } watch-потоков переподключаются
       *[other] { $count } watch-потока переподключаются
    }
status-sync-both =
    { $loading ->
        [one] { $loading } watch-поток загружается
        [few] { $loading } watch-потока загружаются
        [many] { $loading } watch-потоков загружаются
       *[other] { $loading } watch-потока загружаются
    }, { $reconnecting ->
        [one] { $reconnecting } переподключается
       *[other] { $reconnecting } переподключаются
    }

status-operations =
    { $count ->
        [one] Выполняется { $count } операция
        [few] Выполняются { $count } операции
        [many] Выполняется { $count } операций
       *[other] Выполняется { $count } операции
    }
status-operations-tooltip = Показать операции: время, фаза и вывод

status-forwards =
    { $count ->
        [one] { $count } переадресация портов
        [few] { $count } переадресации портов
        [many] { $count } переадресаций портов
       *[other] { $count } переадресации портов
    }
# Разделы навигации пока не переведены: ссылаемся на их английские имена.
status-forwards-tooltip = Открыть Сеть → Переадресация портов

status-terminal = Терминал
status-terminal-tooltip = Новый локальный терминал с этим контекстом (Ctrl+`)

status-mcp-off = MCP выкл.
status-mcp-failed = MCP: ошибка
status-mcp-pending = MCP · ждут подтверждения: { $count }
status-mcp-no-sessions = MCP · нет сессий
status-mcp-sessions =
    { $count ->
        [one] MCP · { $count } сессия
        [few] MCP · { $count } сессии
        [many] MCP · { $count } сессий
       *[other] MCP · { $count } сессии
    }
status-mcp-tooltip = Сессии ИИ-агентов

status-version-tooltip = Скопировать версию Kubach, ОС и версию кластера
status-version-copied = Версия скопирована в буфер обмена
