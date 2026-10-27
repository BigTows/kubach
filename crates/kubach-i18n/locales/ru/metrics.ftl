# Метрики: блок Metrics в деталях, колонки потребления в таблицах Pods и
# Nodes, Prometheus контекста и экран Overview. metrics-server, Prometheus,
# kube-state-metrics, Pods, Nodes, фазы pod (Running, Pending), периоды
# («15m»), величины («250m», «512Mi») и адреса Prometheus — данные, не
# переводятся.

## Блок метрик

metrics-memory = Память
metrics-network = Сеть
metrics-filesystem = Файловая система
metrics-line-working-set = Рабочий набор
metrics-line-receive = Приём
metrics-line-transmit = Передача
metrics-line-read = Чтение
metrics-line-write = Запись
metrics-total = Всего
metrics-by-container = По контейнерам
metrics-by-pod = По pod
metrics-now = сейчас
metrics-request = Запрос
metrics-limit = Лимит
metrics-allocatable = Allocatable
metrics-looking-for-prometheus = Поиск Prometheus…
metrics-no-source = Нет метрик: не найдены ни Prometheus, ни metrics-server
metrics-needs-prometheus = Нужен Prometheus: metrics-server сообщает только CPU и память
metrics-waiting-server = Ожидание metrics-server…
metrics-server-no-answer = metrics-server не ответил
metrics-server-no-sample = У metrics-server ещё нет данных для этого объекта
metrics-loading-prometheus = Загрузка из Prometheus…
metrics-prometheus-no-answer = Prometheus не ответил
metrics-no-data = В Prometheus нет данных за этот период
metrics-server-while-open = metrics-server · пока панель открыта
metrics-unavailable = метрики недоступны
metrics-coverage-less =
    { $subject ->
        [pods] В Prometheus меньше { $range } данных по этим pod
       *[object] В Prometheus меньше { $range } данных по этому объекту
    }
metrics-coverage-same =
    { $subject ->
        [pods] В Prometheus только { $age } данных по этим pod: то же, что { $range }
       *[object] В Prometheus только { $age } данных по этому объекту: то же, что { $range }
    }
metrics-live-collected =
    { $place ->
        [screen] Собрано с открытия экрана: пока { $age }
       *[panel] Собрано с открытия панели: пока { $age }
    }

## Prometheus контекста

prometheus-doesnt-answer = Prometheus { $endpoint } не отвечает
prometheus-invalid-address = Неверный адрес Prometheus: { $address }
prometheus-unusual-namespace = Prometheus { $endpoint } находится в необычном namespace: не используется, пока вы не подтвердите
prometheus-use-it = Использовать
prometheus-use-it-tooltip = Использовать этот Prometheus для контекста
prometheus-detect-again = Найти снова
prometheus-settings = Настройки
prometheus-cannot-connect = не удаётся подключиться: { $error }

## Экран Overview

overview-all-namespaces = все namespace
overview-namespaces = { $count } namespace
overview-not-allowed = нет прав на список
overview-cant-list = не удаётся получить список
overview-ready = готовы
overview-not-ready = не готовы
overview-whole-cluster = весь кластер
overview-restarts =
    { $count ->
        [one] { $count } перезапуск
        [few] { $count } перезапуска
        [many] { $count } перезапусков
       *[other] { $count } перезапуска
    }
overview-restarts-recent =
    { $count ->
        [one] { $count } перезапуск · { $recent } за последние { $range }
        [few] { $count } перезапуска · { $recent } за последние { $range }
        [many] { $count } перезапусков · { $recent } за последние { $range }
       *[other] { $count } перезапуска · { $recent } за последние { $range }
    }
overview-restarts-since =
    { $count ->
        [one] { $count } перезапуск · { $since } с открытия экрана
        [few] { $count } перезапуска · { $since } с открытия экрана
        [many] { $count } перезапусков · { $since } с открытия экрана
       *[other] { $count } перезапуска · { $since } с открытия экрана
    }
overview-of-slots = слотов
overview-pods-of = { $count } / { $capacity } pod
overview-in-scope = в { $scope }
overview-pod-slots-note = Слоты pod: allocatable pod всех узлов (весь кластер)
overview-used = занято
overview-requested = запрошено
overview-unlimited =
    { $count ->
        [one] { $count } pod без лимита
       *[other] { $count } pod без лимита
    }
overview-limits = Лимиты
overview-segment-used = Занято
overview-requests = Запросы
overview-used-of-allocatable = занято от allocatable
overview-usage-unknown = потребление неизвестно
overview-pods-scope = pod: { $scope }
overview-usage-from = Потребление из { $source } · максимум: allocatable, весь кластер
overview-max-allocatable = Максимум: allocatable, весь кластер
overview-cores = { $value } / { $max } ядер
overview-usage-server-no-answer = Потребление недоступно: metrics-server не ответил
overview-usage-loading-prometheus = Загрузка потребления из Prometheus…
overview-usage-prometheus-no-answer = Потребление недоступно: Prometheus не ответил
overview-usage-no-samples = Потребление недоступно: в Prometheus нет данных по pod в { $scope }
overview-usage-see-problem = Потребление недоступно: см. проблему с Prometheus выше
overview-usage-none = Потребление недоступно: установите metrics-server или задайте Prometheus в «Настройки → Кластеры»
overview-cpu-memory = CPU и память
overview-history = Pod в { $scope }
overview-history-prometheus = Pod в { $scope } · Prometheus · { $endpoint }
overview-history-session = Pod в { $scope } · metrics-server · пока экран открыт
overview-pods-by-phase = Pod по фазам
overview-phases-prometheus = { $scope } · kube-state-metrics в Prometheus
overview-phases-session = { $scope } · из кэша, пока экран открыт
overview-phases-collecting = Собираем число pod: график растёт, пока экран открыт
overview-metrics-unavailable = Метрики недоступны
overview-metrics-unavailable-description = Для этого контекста нет ни metrics-server, ни Prometheus: потреблению и графикам нужен один из них. Счётчики из кластера работают.
overview-prometheus-settings = Настройки Prometheus
overview-cluster = Кластер
overview-context = Контекст
overview-user = Пользователь
overview-server-version = Версия сервера
overview-api-resources-title = Ресурсы API
overview-discovering = Поиск…
overview-api-resources = { $kinds } kind в { $groups } группах
overview-custom-resources = Custom resources
