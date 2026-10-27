# Экран Topology: граф Ingress, Service, рабочих нагрузок и их зависимостей,
# слои сетевых политик и трафика, экспорт. Названия kind (Ingress, Service,
# Deployment), «ingress»/«egress», имена объектов, селекторы меток, типы
# Service (headless, NodePort) и правила NetworkPolicy — данные, не
# переводятся.

## Фильтры и панель инструментов

topology-search-placeholder = Найти узел…
topology-labels-placeholder = Метки: app=web,tier!=db
topology-detail-workloads = Нагрузки
topology-detail-workloads-tooltip = Pod свёрнуты в свою рабочую нагрузку
topology-detail-pods = Pods
topology-detail-pods-tooltip = Каждый pod — отдельный узел
topology-class-routes = Маршруты
topology-class-workloads = Нагрузки
topology-class-external = Внешние
topology-class-hide = Скрыть
topology-class-show = Показать
topology-size =
    { $nodes ->
        [one] { $nodes } узел
        [few] { $nodes } узла
        [many] { $nodes } узлов
       *[other] { $nodes } узла
    } · { $edges ->
        [one] { $edges } связь
        [few] { $edges } связи
        [many] { $edges } связей
       *[other] { $edges } связи
    }
topology-no-matches = Совпадений нет
topology-match = Совпадение { $position } из { $count }
topology-policies = Сетевые политики
topology-policies-tooltip = Показать, что изолируют и блокируют NetworkPolicy. { $note }
topology-issues = Проблемы · { $count }
topology-issues-tooltip = Проблемы, найденные в графе
topology-broken-references =
    { $count ->
        [one] { $count } битая ссылка
        [few] { $count } битые ссылки
        [many] { $count } битых ссылок
       *[other] { $count } битой ссылки
    }

## Нечего показать

topology-nothing = Нечего показать
topology-nothing-filtered = Нет объектов, подходящих под фильтры.
topology-nothing-selected-namespaces = В выбранных namespace нет Ingress, Service и рабочих нагрузок.
topology-nothing-any = Нет Ingress, Service и рабочих нагрузок.
topology-error-no-access = Нет доступа к { $kinds }: в графе их нет.
topology-error-cant-refresh = Не удаётся обновить { $kinds }, показаны данные из кэша. Повтор…

## Слой сетевых политик

topology-policies-scope = Анализируются только стандартные NetworkPolicy networking.k8s.io/v1 (без CRD Cilium, Calico и других); применяются ли они, зависит от CNI.
topology-policies-loading = Сетевые политики: загрузка…
topology-policies-none = Сетевые политики: в выбранных namespace их нет.
topology-policies-count =
    { $count ->
        [one] Сетевые политики: { $count } политика.
        [few] Сетевые политики: { $count } политики.
        [many] Сетевые политики: { $count } политик.
       *[other] Сетевые политики: { $count } политики.
    }
topology-policies-note = { $lead } Изолированные нагрузки отмечены щитом; красные пунктирные связи — трафик Service, заблокированный политикой, зелёные точечные — то, что политика явно разрешает. { $note }
topology-policy-section = Сетевая политика
topology-not-isolated = Не изолирован: весь { $direction } разрешён
topology-isolated-by = Изолирован политиками { $policies }
topology-policy-deny-all = { $direction }: всё запрещено
topology-reach = Есть ли доступ к…
topology-reach-target = Цель
topology-reach-target-label = Цель доступа
topology-reach-search = Найти нагрузку или Service…
topology-port-placeholder = Порт: 8080, http, 53/udp
topology-port-empty = Введите порт
topology-port-invalid = Не порт: { $text }
topology-port-out-of-range = Порт вне диапазона: { $port }
topology-reach-no-pods = Service не выбирает ни одного pod: доступаться не к чему.
topology-reach-allowed = Разрешено
topology-reach-blocked = Заблокировано
topology-reach-via = { $port } · через { $via }
topology-reach-egress = Egress этой нагрузки
topology-reach-ingress = Ingress цели

## Граф

topology-presence-missing = отсутствует
topology-presence-unknown = неизвестно
topology-isolated = изолирован
topology-isolated-in = изолирован вход
topology-isolated-out = изолирован выход
topology-edge-allowed = разрешено
topology-edge-allows = разрешает { $ports }
topology-edge-blocked = заблокировано политикой
topology-edge-ready = { $count } готово
topology-edge-ready-of = { $count }/{ $total } готово
topology-traffic-requests = { $rate } запр/с
topology-traffic-flows = { $rate } потоков/с
topology-traffic-errors = { $percent }% ошибок
topology-traffic-dropped = { $percent }% отброшено
topology-traffic-idle = нет трафика
topology-edge-routes-to = направляет в
topology-edge-selects = выбирает
topology-edge-owns = владеет
topology-edge-mounts = монтирует
topology-edge-env-from = env из
topology-edge-service-account = service account
topology-edge-exposes = публикует
topology-edge-tls = TLS-секрет
topology-edge-policy-allows = политика разрешает
topology-edge-traffic = трафик
topology-edge-links-to = ссылается на
topology-diagnostic-no-ready = У { $node } нет готовых endpoints
topology-diagnostic-not-ready = У { $node } нет готовых endpoints (не готово: { $count })
topology-diagnostic-selector = Селектор { $node } не выбирает ни одного pod
topology-diagnostic-missing-target = { $node } ссылается на отсутствующий { $target }
topology-diagnostic-missing-port = { $node } ссылается на порт { $port }, которого нет у { $target }

## Проблемы и детали

topology-no-problems = Проблем не найдено.
topology-open-details = Открыть детали
topology-problems = Проблемы
topology-overview = Обзор
topology-fact-pods-ready = Готовые pod
topology-of = { $count } из { $total }
topology-fact-replicas = Реплики
topology-fact-phase = Фаза
topology-fact-cluster-ip = Cluster IP
topology-fact-external-name = Внешнее имя
topology-fact-ports = Порты
topology-fact-hosts = Хосты
topology-fact-class = Класс
topology-show-on-graph = Показать на графе
topology-incoming = Входящие
topology-outgoing = Исходящие
topology-missing = Отсутствует
topology-not-loaded = Не загружен

## Слой трафика

traffic-button = Трафик
traffic-button-tooltip = Показать измеренный трафик по метрикам Prometheus из Istio, Linkerd или Cilium Hubble
traffic-detect-again-tooltip = Снова поискать метрики трафика в Prometheus
traffic-needs-prometheus = Для трафика нужен Prometheus с метриками mesh или CNI: { $reason }.
traffic-prometheus-off = Prometheus выключен для этого контекста в настройках
traffic-prometheus-looking = идёт поиск Prometheus…
traffic-prometheus-not-found = Prometheus в кластере не найден
traffic-prometheus-no-address = адрес Prometheus не задан в настройках
traffic-prometheus-unavailable = Prometheus недоступен
traffic-detecting = Трафик: поиск метрик Istio, Linkerd или Cilium Hubble…
traffic-no-sources = Нет источников метрик трафика: Istio, Linkerd и Cilium Hubble в кластере не установлены. На графе только объявленные связи.
traffic-no-metrics =
    { $count ->
        [one] Нет источников метрик трафика: { $sources } установлен, но в Prometheus нет его метрик трафика. На графе только объявленные связи.
       *[other] Нет источников метрик трафика: { $sources } установлены, но в Prometheus нет их метрик трафика. На графе только объявленные связи.
    }
traffic-window-tooltip = Скорость за последние { $window }
traffic-lead-loading = Трафик: { $source } · загрузка…
traffic-lead-none = Трафик: { $source } · за этот период трафика нет
traffic-lead =
    { $count ->
        [one] Трафик: { $source } · { $count } поток
        [few] Трафик: { $source } · { $count } потока
        [many] Трафик: { $source } · { $count } потоков
       *[other] Трафик: { $source } · { $count } потока
    }
traffic-lead-busiest =
    { $count ->
        [one] Трафик: { $source } · { $count } поток (показаны самые нагруженные)
        [few] Трафик: { $source } · { $count } потока (показаны самые нагруженные)
        [many] Трафик: { $source } · { $count } потоков (показаны самые нагруженные)
       *[other] Трафик: { $source } · { $count } потока (показаны самые нагруженные)
    }
traffic-also-found = Найдено также: { $sources } (не используется).
traffic-poll-failed = Последний опрос не удался: { $error }

## Экспорт

topology-export = Экспорт
topology-export-tooltip = Экспортировать граф в текущем виде
topology-export-mermaid = Скопировать Mermaid
topology-export-dot = Скопировать Graphviz DOT
topology-export-svg = Сохранить SVG…
topology-export-png = Сохранить PNG…
topology-export-copied = Топология скопирована как { $format }
topology-export-saved = Сохранено: { $path }
topology-export-save-failed = Не удалось сохранить { $path }: { $error }
