# 0009 — Источники метрик: metrics-server сейчас, Prometheus для истории

Статус: принято

## Контекст
Текущее потребление CPU/Memory уже берётся из metrics-server (`metrics.k8s.io`, #71): это
только последний срез, без истории. Для графиков за 1ч/6ч/24ч (#100) нужен источник с
временными рядами. Агент в кластере ставить нельзя (ADR 0004), свой сборщик метрик в
приложении бессмыслен — история нужна и за время, когда Kubach был закрыт. Во многих
кластерах уже стоит Prometheus (чаще всего kube-prometheus-stack или community-чарт
`prometheus`), который собирает cAdvisor с kubelet.

## Решение
- Два источника: metrics-server — текущее значение в таблицах (без него — Prometheus, см.
  ниже); в деталях — блок «Metrics»
  в самом верху (#146) с табами CPU / Memory / Network / Filesystem. Если Prometheus найден,
  блок рисует историю из него (15m / 1h / 6h / 24h / 7d). Без Prometheus блок копит CPU и память из
  опросов metrics-server на клиенте, пока панель открыта (график растёт со временем, до часа
  при опросе раз в 15 с); Network и Filesystem — только с Prometheus, без него эти табы
  приглушены с подсказкой. Ни того, ни другого — все табы приглушены.
- Диапазоны (#376). Prometheus: вместе с обновлением графиков (раз в минуту), пока хоть один
  диапазон недоступен, — один мгновенный запрос о самой старой точке объекта,
  `min_over_time(timestamp(<память объекта>)[7d:5m])` (2016 вычислений маленького
  селектора — дёшево; точность 5 минут); когда доступны все — больше не спрашивает. Диапазон доступен, если данные уходят
  дальше предыдущего, более короткого: иначе он нарисовал бы те же точки, прижатые к правому
  краю; 15m доступен всегда. Запрос отвечает сразу и про retention, и про возраст пода;
  `prometheus_tsdb_lowest_timestamp` есть, только если Prometheus скрейпит сам себя, и
  описывает весь TSDB, а настроенный retention ничего не знает о поде, созданном 10 минут
  назад. Недоступные диапазоны выключены (не выбираются) с подсказкой «Prometheus has only
  40m of data for this object: same as 1h»; если выбранный по умолчанию 1h недоступен и
  пользователь сам ничего не выбирал, блок переходит на самый длинный доступный. Ошибка
  запроса — все диапазоны доступны. metrics-server: 5m / 15m / All по накопленной истории;
  окно доступно, когда накоплено не меньше него («Collected since the panel opened: 3m so
  far»), All — всегда.
- Наведение на график (#376): вертикальная линия по ближайшей точке, маркеры на сериях и
  плашка со временем (локальное `HH:MM:SS` и «-5m» от правого края) и значениями всех серий,
  Request и Limit в формате метрики. Состояние наведения — в элементе графика; view
  перерисовывается, только когда курсор переходит к другой точке или уходит с графика.
- Блок есть у Pod, Deployment, StatefulSet, DaemonSet, ReplicaSet, Job и Node. Requests и
  limits (сумма по текущим подам; у Node — allocatable) — пунктирные линии на суммарном
  графике; линия намного выше данных (более чем в 4 раза) остаётся только в легенде со
  стрелкой, чтобы не сплющивать график. Pod с несколькими контейнерами — разбивка по
  контейнерам (`sum by (container)`), workload — по подам (`sum by (pod)`); только CPU и
  память: сеть принадлежит поду, а не контейнеру.
- Источник Prometheus выбирается на контекст в Settings → Clusters → Prometheus (#180):
  **Auto** — лучший найденный в кластере; **выбранный из найденных** (`namespace/service:port`);
  **свой адрес** — `http(s)://host[:port][/path]` (запрос напрямую с машины пользователя) или
  `namespace/service[:port]` (сервис в кластере); **Off** — ни поиска, ни запросов. Выбор
  хранится в настройках: `[clusters.prometheus_sources]` (`"off"`, `"manual"`, `"auto"`,
  `{ found = "…" }`), адрес — в `[clusters.prometheus]`, как раньше. Контекст без записи в
  `prometheus_sources` с адресом — Manual (обратная совместимость), без адреса — Auto.
- Автопоиск: список Service во всех namespace (при запрете — в известных:
  `monitoring`, `prometheus`, …), ранжирование по меткам и именам (kube-prometheus-stack,
  `prometheus-operated`, `prometheus-k8s`, `prometheus-server`, `app.kubernetes.io/name=prometheus`),
  параллельная проверка до 8 первых кандидатов запросом `/api/v1/query` с таймаутом 8 с.
  Кандидаты из известных namespace (`WELL_KNOWN_NAMESPACES`: `monitoring`, `prometheus`,
  `kube-prometheus`, `kube-prometheus-stack`, `observability`, `kube-system`,
  `openshift-monitoring`, `cattle-monitoring-system`) всегда выше остальных при любом score,
  внутри группы — по score, затем стабильно по (namespace, имя) (#200). Иначе Service с
  подходящей меткой или именем в любом namespace, где пользователь кластера может создавать
  Service (например `aaa-dev`), выигрывал у настоящего при равном score и подменял CPU/Memory
  в таблицах.
- Если Auto нашёл серверы **только вне известных namespace**, он к ним не подключается:
  статус «ждёт подтверждения», в Settings → Clusters → Prometheus — предупреждение и кнопка
  «Use it», в подписи над таблицей и в блоке метрик — «Prometheus ns/svc:port is in an unusual
  namespace: not used until confirmed» (в блоке метрик — тоже с «Use it»). «Use it» выбирает
  этот сервер как найденный (`{ found = "…" }`), дальше он используется как любой выбранный.
  В списке выбора такие серверы помечены «(unusual namespace)». Выбранный вручную или
  введённый адрес не проверяется: это решение пользователя. Сохранённый до #200 результат
  поиска тоже читается так: Auto берёт первый сервер из известного namespace, а не первый в
  списке.
  Результат — все ответившие кандидаты по порядку (или «не найден») со временем поиска —
  сохраняется в `state.json` (`prometheus`, #153). Поиск идёт **один раз**: при первом
  подключении к контексту с Auto без сохранённого результата; дальше, в том числе после
  перезапуска, используется сохранённый. Заново — только кнопкой «Detect again» (в настройках
  или в блоке метрик). Неудачный поиск (кластер недоступен, нет прав на список Service) не
  сохраняется. Модель выбора и приоритет — `metrics::prometheus::source` в `kubach-core` с тестами.
- Если выбранный или сохранённый адрес не отвечает, повторного поиска «молча» нет: блок метрик
  показывает «Prometheus at … doesn't answer» с действиями «Detect again» и «Settings».
- Подключения держит общий реестр приложения (`PrometheusRegistry`) для всех подключённых
  контекстов; таблицы и панель деталей контекста делят одно подключение. Подключение
  сбрасывается, когда контекст отключается или переподключается.
- Сервисы в кластере — только через API proxy kube (`/api/v1/namespaces/…/services/…/proxy`)
  с учётными данными контекста: без port-forward и без открытия Prometheus наружу. Нужно
  право `get services/proxy`.
- Pod и workloads — PromQL по рядам cAdvisor (`container_cpu_usage_seconds_total`,
  `container_memory_working_set_bytes`, `container_network_{receive,transmit}_bytes_total`,
  `container_fs_{reads,writes}_bytes_total`) по `namespace`/`pod`; поды workload — по имени:
  Deployment `<имя>-<hash>-<suffix>`, StatefulSet `<имя>-<ordinal>`, DaemonSet и ReplicaSet
  `<имя>-<5 символов>`, Job `<имя>-[<index>-]<5 символов>`. CPU, память и диск — только
  контейнеры (`container!=""`, `!="POD"`, без `/metrics/resource`): cgroup пода и sandbox
  иначе удваивают сумму. Сеть считается на sandbox пода, без фильтра по контейнеру. Node —
  диск по `node_disk_{read,written}_bytes_total`.
- Node — node-exporter (`node_cpu_seconds_total`, `MemTotal - MemAvailable`,
  `node_network_*` без `lo`/`veth`/мостов), привязка к ноде через
  `node_uname_info{nodename}`; kube-prometheus-stack не хранит корневой cgroup kubelet.
  Запасной вариант — cAdvisor `id="/"` по метке `node` или `instance`. 120 точек на диапазон, окно
  `rate` — не меньше двух шагов и 2 минут. Построение запросов и разбор `query_range` —
  в `kubach-core` (`metrics::prometheus`) с тестами.
- Колонки CPU / Memory таблиц Pods и Nodes (#179): главный источник — metrics-server. Если
  кластер не отдаёт `metrics.k8s.io`, таблица берёт значения из того же Prometheus, что и
  панель деталей (одно подключение на контекст, без отдельного поиска). На опрос — два
  мгновенных запроса (`/api/v1/query`) сразу для всех видимых namespace, а не запрос на объект:
  `sum by (namespace, pod, container)` от `rate(container_cpu_usage_seconds_total[2m])` и от
  `container_memory_working_set_bytes` с тем же фильтром контейнеров и `namespace=~"a|b"`;
  Node — `sum by (node)` по node-exporter через `node_uname_info{nodename}`, запасной вариант —
  корневой cgroup cAdvisor по `node`/`instance`. Интервал опроса (15 с), единицы (наноядра,
  байты) и сортировка — как у metrics-server. Над таблицей видно, откуда значения:
  «metrics-server», «Prometheus» или «metrics unavailable». Запросы и разбор —
  `metrics::prometheus::usage` в `kubach-core`.
- Экран Overview (#414): карточки Nodes (Ready/всего, весь кластер), Pods (кольцо Running + Pending из ёмкости нод, по фазам,
  рестарты, ёмкость нод), CPU и Memory (использование / requests / limits против allocatable
  кольцами `Donut` из kubach-ui: used, requests, limits, в центре процент, под кольцом «X / max»; без usage — строка, почему его нет и что сделать); графики CPU и памяти с порогами Requests и
  Allocatable и график Pods по фазам. Счётчики — из кэша объектов: Pods выбранных namespace и
  все Nodes, подписка только пока экран показан (watch общие с таблицами, `subscribe`
  разделяет их); подсчёт — в фоне. Источники как у таблиц и деталей
  (`metrics::overview::sources`): текущее использование — metrics-server (сумма подов
  выбранных namespace из общего опроса), без него — Prometheus; история — Prometheus с
  диапазонами и доступностью как в деталях (самая старая точка памяти подов выбранных
  namespace), без него — опросы metrics-server, пока экран открыт. PromQL —
  `metrics::prometheus::overview`: `sum(rate(container_cpu_usage_seconds_total{…}))` и
  `sum(container_memory_working_set_bytes{…})` с фильтром контейнеров и `namespace=~`,
  фазы — `sum by (phase) (kube_pod_status_phase{…})` (kube-state-metrics), рестарты за
  диапазон — `increase(kube_pod_container_status_restarts_total[…])`. Без kube-state-metrics
  фазы копятся из кэша, пока экран открыт. Allocatable и Nodes помечены «whole cluster»,
  остальное считается по выбранным namespace. Ни metrics-server, ни Prometheus — плашка
  «Metrics unavailable» со ссылкой на настройки Prometheus, счётчики остаются. Скрытый экран
  ничего не опрашивает и не держит watch.
- Нет Prometheus, он выключен (Off) или не отвечает — блок работает от metrics-server, как
  описано выше. История на клиенте не сохраняется: новое выделение — новый график.

## Последствия
- Долгая история есть только там, где Prometheus уже собирает cAdvisor; без него — только
  CPU и память с момента открытия панели.
- Workloads сопоставляются с подами по имени, без kube-state-metrics: поды с похожими
  именами от другого владельца теоретически могут попасть в график. Requests/limits и
  выборка metrics-server берут текущие поды по селектору из кэша.
- Нестандартные метки (переразметка `node`/`instance`, другой job) могут дать пустые графики —
  тогда виден «No data», а не ошибка. Настраиваемые запросы — отдельной задачей при необходимости.
- Значения из Prometheus в таблице сглажены окном `rate` в 2 минуты и отстают от
  metrics-server на интервал скрейпа; память Node по node-exporter (`MemTotal - MemAvailable`)
  может немного отличаться от working set корневого cgroup, который даёт metrics-server.
  Если `metrics.k8s.io` объявлен, но не отвечает, таблица на Prometheus не переключается.
- Prometheus за аутентификацией (OAuth-прокси, basic auth) по URL пока не поддерживается;
  через API proxy работает любой сервис, доступный контексту.
- Prometheus в нестандартном namespace под Auto требует одного подтверждения «Use it»; при
  этом подделка в известном namespace (`kube-system`, `monitoring`) по-прежнему возможна, но
  создавать там Service обычно может только администратор кластера.
- Сохранённый результат поиска устаревает: Prometheus, поставленный в кластер позже, не
  найдётся сам — нужен «Detect again». Это плата за отсутствие повторных поисков при каждом
  запуске (список всех Service в большом кластере дорог).
