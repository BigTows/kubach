# Нижняя панель: вкладки логов и терминалов, локальный терминал, shell на
# ноде, диалог переадресации порта, экран «Переадресация портов» и кнопки
# в деталях. Имена pod, контейнеров, namespace, контекстов, нод и портов,
# селекторы и образы — данные. Термины Kubernetes (Pod, Service,
# init-контейнер, port-forward, kubectl) не переводятся и не склоняются.

## Общее

dock-connecting = Подключение…
dock-not-connected = Нет подключения к кластеру
dock-context-not-connected = Контекст { $context } не подключён; подключитесь к нему, чтобы переподключить вкладку
dock-cancel = Отмена
dock-container = Контейнер { $container }

## Полоса вкладок

dock-tab-close = Закрыть
dock-collapse = Свернуть
dock-expand = Развернуть
dock-logs-streaming =
    { $lines ->
        [one] Поток · { $lines } строка
        [few] Поток · { $lines } строки
        [many] Поток · { $lines } строк
       *[other] Поток · { $lines } строки
    }
dock-logs-ended =
    { $lines ->
        [one] Завершён · { $lines } строка
        [few] Завершён · { $lines } строки
        [many] Завершён · { $lines } строк
       *[other] Завершён · { $lines } строки
    }

## Кнопки потоков в деталях

dock-forward-title = Переадресация порта
dock-entry-logs-all = Логи: поток логов всех контейнеров в нижней панели
dock-entry-logs = Логи: поток логов в нижней панели
dock-entry-shell-default = Shell: открыть shell в { $container } (контейнер по умолчанию)
dock-entry-shell = Shell: открыть shell в контейнере
dock-entry-debug-default = Отладка: запустить эфемерный контейнер busybox для { $container } (контейнер по умолчанию) и подключиться к нему
dock-entry-debug = Отладка: запустить эфемерный контейнер busybox рядом с приложением и подключиться к нему
dock-entry-forward-selector = Переадресация порта: локальный порт на готовый pod этого ресурса
dock-entry-forward = Переадресация порта: локальный порт на этот pod
dock-entry-forward-port = Переадресация порта: порт { $port }, выбор локального порта
dock-choose-container = Выбрать контейнер
dock-choose-port = Выбрать порт
dock-choose-local-port = Выбрать локальный порт…

## Меню контейнеров

dock-all-containers = Все контейнеры
dock-containers = Контейнеры
dock-init-containers = Init-контейнеры

## Подтверждение отладки

dock-debug-title = Отладить pod { $pod }?
dock-debug-message = В pod { $namespace }/{ $pod } будет добавлен эфемерный отладочный контейнер ({ $image }). Удалить его нельзя: он останется в pod, пока pod не удалят.
dock-debug-message-target = В pod { $namespace }/{ $pod } будет добавлен эфемерный отладочный контейнер ({ $image }) с общими процессами с { $container }. Удалить его нельзя: он останется в pod, пока pod не удалят.
dock-debug-dont-ask = Больше не спрашивать
dock-debug-confirm = Добавить отладочный контейнер

## Вкладка логов

dock-logs-search = Поиск
dock-logs-no-containers = Нет контейнеров для чтения
dock-logs-waiting = Ожидание строк лога…
dock-logs-no-lines = Нет строк лога
dock-logs-loading-earlier = Загрузка более ранних строк…
dock-logs-earlier-failed = Не удалось загрузить более ранние строки: { $error }
dock-logs-beginning = Начало лога
dock-logs-capped = Показаны последние строки (до { $lines } строк или { $mib } МиБ); более ранние смотрите через kubectl logs
dock-logs-reconnecting = Переподключение…
dock-logs-dropped =
    { $count ->
        [one] Пропущена { $count } строка
        [few] Пропущены { $count } строки
        [many] Пропущено { $count } строк
       *[other] Пропущено { $count } строки
    }
dock-logs-ended-indicator = Поток завершён
dock-logs-previous = Предыдущий
dock-logs-timestamps = Время
dock-logs-wrap = Перенос
dock-logs-prev-match = Предыдущее совпадение (Shift+Enter)
dock-logs-next-match = Следующее совпадение (Enter)
dock-logs-pause = Пауза
dock-logs-follow = Следить
dock-logs-copy = Скопировать все строки
dock-logs-clear = Очистить

## Терминалы shell и отладки

dock-terminal-running = Работает
dock-terminal-starting-debug = Запуск отладочного контейнера…
dock-terminal-attached = Подключено к отладочному контейнеру { $container }. Если приглашения не видно, нажмите Enter.
# $shells: /bin/bash, /bin/sh, /bin/ash, /busybox/sh.
dock-terminal-no-shell = В контейнере { $container } нет shell: не найден ни один из { $shells } (образ distroless или scratch?). Используйте «Отладку», чтобы подключить контейнер busybox
# Первой оболочки из списка нет; $shell: /bin/sh, $missing: /bin/bash.
dock-terminal-shell-fallback = Запущен { $shell }: в образе нет { $missing }
dock-terminal-debug-tooltip = Подключить эфемерный контейнер busybox, общий по процессам с этим контейнером
dock-terminal-container-tooltip = Контейнер: при смене shell переподключится
dock-terminal-target-tooltip = Целевой контейнер: при смене запустится новый отладочный контейнер
dock-terminal-reconnect = Переподключить
dock-terminal-new-debug = Новый отладочный контейнер
dock-exit-success = Завершён
dock-exit-code = Завершён с кодом { $code }
dock-exit-failed = Ошибка
dock-exit-failed-message = Ошибка: { $message }
dock-exit-disconnected = Отключён

## Локальный терминал

dock-local-title = Терминал — { $context }/{ $namespace }
dock-local-title-plain = Терминал
dock-local-starting = Запуск shell…
dock-local-context-unavailable = Контекст { $context } недоступен (хранилище заблокировано?)
dock-local-no-cluster = В Kubach не открыт кластер: KUBECONFIG остаётся таким, как его задаёт ваш shell.
dock-local-no-temp-dir = Нет папки для временного kubeconfig
dock-local-temp-write-failed = Не удалось записать временный kubeconfig: { $error }
dock-local-restart = Перезапустить

## Shell на ноде

dock-node-title = Shell на ноде — { $node }
dock-node-creating = Создание pod…
dock-node-creating-in = Создание pod в { $namespace }…
dock-node-created = Создан привилегированный pod { $namespace }/{ $pod }; ждём запуска…
dock-node-waiting = Ожидание pod…
dock-node-new-pod = Новый pod

## Диалог переадресации и уведомления

dock-forward-enter-port = Введите порт
dock-forward-invalid-port = { $text } — не порт (1–65535)
dock-forward-menu-tcp-only = { $port } (port-forward работает только с TCP)
dock-forward-started = Переадресация порта запущена
dock-forward-started-via = { $route } (pod { $pod }:{ $port })
dock-forward-open = Открыть
dock-forward-open-in-browser = Открыть { $url } в браузере
dock-forward-copy = Копировать
dock-forward-copy-tooltip = Скопировать адрес
dock-forward-service-port = Порт Service
dock-forward-pod-port = Порт pod
dock-forward-service-ports = Порты Service
dock-forward-container-ports = Порты контейнеров
dock-forward-local-placeholder = Случайный свободный порт
dock-forward-route = → pod { $pod }:{ $port }
dock-forward-port-not-tcp = Порты { $protocol } нельзя переадресовать: port-forward работает только с TCP
dock-forward-service-to-pod-port = Порт Service → порт pod { $port }
dock-forward-service-to-named-port = Порт Service → порт контейнера с именем { $name }
dock-forward-looking = Поиск готового pod…
dock-forward-ready-pod = Готовый pod: { $pod }
dock-forward-pod = Pod: { $pod }
dock-forward-pods = Pod: { $selector }
dock-forward-udp-disabled = Порты UDP недоступны: port-forward туннелирует только TCP.
dock-forward-local-port = Локальный порт на 127.0.0.1
dock-forward-local-random = Пусто: случайный свободный порт. Введённый порт запоминается на следующий раз.
dock-forward-local-remembered = Этот порт запомнится для этой переадресации.
dock-forward-port-busy = Порт { $port } уже занят на этом компьютере. Введите другой порт или оставьте поле пустым для случайного.
dock-forward-port-denied = Порт { $port } нельзя занять без прав администратора. Введите порт выше 1024 или оставьте поле пустым.
dock-forward-start = Запустить
dock-forward-starting = Запуск…

## Экран «Переадресация портов»

dock-forwards-ready = Работает
dock-forwards-failing = Сбой
dock-forwards-reconnecting = Переподключение
dock-forwards-ready-tooltip = Локальный порт слушает; последнее соединение дошло до pod
dock-forwards-failing-tooltip = Последнее соединение с pod не удалось. Локальный порт по-прежнему слушает: следующее соединение попробует снова
dock-forwards-reconnecting-tooltip = После сбоя новое соединение пробует достучаться до pod
dock-forwards-last-error = Последняя ошибка: { $error }
dock-forwards-stop-tooltip = Остановить переадресацию и закрыть её соединения
dock-forwards-errors =
    { $count ->
        [one] { $count } ошибка
        [few] { $count } ошибки
        [many] { $count } ошибок
       *[other] { $count } ошибки
    }
dock-forwards-open = Открыть { $url }
dock-forwards-copy = Скопировать { $address }
dock-forwards-description = Переадресации продолжают работать при смене контекста, пока их не остановят.
dock-forwards-pod = Pod
dock-forwards-context-current = Текущий контекст
dock-forwards-connections = { $active } открыто · { $total } всего
dock-forwards-connections-tooltip = Открытые сейчас соединения и все соединения с момента запуска переадресации
dock-forwards-sort = Сортировка
dock-forwards-sort-port = Порт
dock-forwards-sort-status = Статус
dock-forwards-sort-target = Цель
dock-forwards-paused = На паузе
dock-forwards-paused-tooltip = Локальный порт остаётся занят, но ничего не пробрасывается: соединения закрываются
dock-forwards-pause = Пауза: порт остаётся за форвардом, соединения закрываются
dock-forwards-resume = Продолжить проброс на { $address }
dock-forwards-reveal = Открыть { $target }
dock-forwards-empty = Нет переадресаций портов
dock-forwards-empty-description = Выберите pod, Service или workload и нажмите «Переадресация порта».
dock-forwards-restore-waiting = Ожидание
dock-forwards-restore-waiting-tooltip = Работала при выходе из Kubach: запустится снова на том же локальном порту, когда контекст подключится
dock-forwards-restore-waiting-context = Запустится после подключения контекста { $context }
dock-forwards-restore-unknown-context = Контекст { $context } недоступен: разблокируйте хранилище или добавьте его kubeconfig
dock-forwards-restore-starting = Запуск
dock-forwards-restore-starting-detail = Запуск на том же локальном порту…
dock-forwards-restore-failed = Не запущена
dock-forwards-restore-failed-tooltip = Работала при выходе из Kubach, но не запустилась снова. Повторите или уберите её, чтобы она не восстанавливалась
dock-forwards-restore-port-busy = Порт { $port } уже занят на этом компьютере. Освободите его и повторите или уберите эту переадресацию и запустите новую.
dock-forwards-restore-gone = { $source } больше нет в кластере
dock-forwards-restore-cannot-connect = Не удалось подключиться к контексту { $context }: { $error }
dock-forwards-restore-disconnected = Контекст { $context } отключился до запуска переадресации
dock-forwards-retry-tooltip = Попробовать запустить переадресацию снова
dock-forwards-remove-tooltip = Убрать переадресацию: она больше не будет восстанавливаться
