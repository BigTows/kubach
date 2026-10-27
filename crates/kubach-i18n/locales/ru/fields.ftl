# Тексты встроенных плагинов kubach-core в панели деталей: причины состояния и
# заметки, состояния контейнеров и реплик, подписи полей.


## Причины состояния

state-active = Активен
state-available = Доступен
state-ready = Готов
state-scaled-to-zero = Масштабирован до нуля
state-rollout-paused = Выкатка приостановлена
state-waiting-rollout = Ожидание, пока контроллер увидит выкатку
state-waiting-controller = Ожидание контроллера
state-waiting-status = Ожидание статуса
state-replicas-updated = Обновлено реплик: { $updated } из { $desired }
state-old-replicas =
    { $count ->
        [one] { $count } старая реплика ожидает завершения
        [few] { $count } старые реплики ожидают завершения
        [many] { $count } старых реплик ожидают завершения
       *[other] { $count } старой реплики ожидают завершения
    }
state-updated-available = Доступно обновлённых реплик: { $available } из { $updated }
state-replicas-ready = Готово реплик: { $ready } из { $desired }
state-pods-updated = Обновлено Pod: { $updated } из { $desired }
state-pods-available = Доступно Pod: { $available } из { $desired }
state-pods-misscheduled = Ошибочно размещено Pod: { $count }
state-hpa-disabled = Масштабирование выключено: цель масштабирована до нуля
state-hpa-waiting-metrics = Ожидание первых метрик
state-hpa-limited = Ограничено: { $reason }
state-hpa-scaling = Масштабирует
state-crd-established = Установлен
state-crd-not-established = Ещё не установлен
state-gateway-accepted = Принят
state-gateway-programmed = Настроен
state-gateway-listener = Listener { $name }: { $reason }
state-route-not-accepted = Ещё не принят ни одним gateway
state-route-waiting-gateways = Ожидание gateway
state-deleting = Удаляется
state-suspended = Приостановлен
state-job-failing = Сбоит
state-cronjob-active = Активных Job: { $count }
state-cronjob-last-failed = Последний запланированный Job завершился неуспешно
state-cronjob-never-succeeded = Ни один запланированный Job ещё не завершился успешно
state-cronjob-not-scheduled = Ещё не запускался
state-cronjob-scheduled = Запланирован
state-service-waiting-lb = Ожидание балансировщика нагрузки
state-ingress-waiting-address = Ожидание адреса
state-node-scheduling-disabled = Готов, планирование выключено
state-pod-containers-ready = Готово контейнеров: { $ready }/{ $total }
state-quota-not-computed = Ещё не посчитана
state-quota-within = В пределах квоты
state-quota-at-limit = Достигнут предел: { $resources }
state-pdb-not-observed = Ещё не обработан
state-pdb-no-pods = Не выбрано ни одного Pod
state-pdb-unhealthy = Исправно Pod: { $current } из { $desired } требуемых
state-pdb-no-disruptions = Прерывания запрещены: вытеснения ждут
state-pdb-disruptions = Допустимо прерываний: { $count }
state-claim-protected = Удаляется: { $finalizer } удерживает claim, пока его использует Pod
state-volume-protected = Удаляется: { $finalizer } удерживает том, пока к нему привязан claim
state-claim-used-by = Удаляется: используется в Pod { $pods }
state-vpa-provided = Рекомендация готова
state-vpa-provided-low = Рекомендация готова (низкая уверенность)
state-vpa-no-recommendation = Рекомендации ещё нет
state-container-running = Работает
state-container-not-ready = Не готов
state-container-unknown = Неизвестно


## Заметки связанных объектов

brief-pod =
    готово { $ready }/{ $total }{ $restarts ->
        [0] {""}
       *[other] {" · "}{ $restarts ->
            [one] { $restarts } перезапуск
            [few] { $restarts } перезапуска
            [many] { $restarts } перезапусков
           *[other] { $restarts } перезапуска
        }
    }
brief-pod-on-node =
    готово { $ready }/{ $total }{ $restarts ->
        [0] {""}
       *[other] {" · "}{ $restarts ->
            [one] { $restarts } перезапуск
            [few] { $restarts } перезапуска
            [many] { $restarts } перезапусков
           *[other] { $restarts } перезапуска
        }
    } · { $node }
brief-revision-ready = ревизия { $revision } · готово { $ready }/{ $desired }
brief-ready = готово { $ready }/{ $desired }
brief-keys =
    { $count ->
        [one] { $count } ключ
        [few] { $count } ключа
        [many] { $count } ключей
       *[other] { $count } ключа
    }


## Состояния, которые описывает приложение

container-state-running = Работает
container-state-waiting = Ожидание: { $reason }
container-state-waiting-message = Ожидание: { $reason } ({ $message })
container-state-terminated = Завершён: { $reason } (код выхода { $code })
container-state-unknown = Неизвестно
container-state-not-ready = { $state }, не готов
replicas-desired = желаемых: { $count }
replicas-current = текущих: { $count }
replicas-ready = готовых: { $count }
replicas-updated = обновлённых: { $count }
replicas-available = доступных: { $count }


## Подписи с номером

field-rule-n = Правило { $n }
field-revision-of = Ревизия { $revision }

## Labels of detail fields, key/value rows and table columns (by their English text)

field-access-modes = Режимы доступа
field-address = Адрес
field-addresses = Адреса
field-affinity = Affinity
field-aggregates = Агрегирует
field-allowed-disruptions = Допустимо прерываний
field-api-discovery-failed = Сбой обнаружения API
field-api-group-version-unreadable = Не читается версия группы API
field-api-groups = Группы API
field-applications-sync-policy = Политика синхронизации приложений
field-architecture = Архитектура
field-artifact-revision = Ревизия артефакта
field-artifact-updated = Артефакт обновлён
field-auto-sync = Автосинхронизация
field-backoff-limit = Лимит повторов
field-binding-mode = Режим привязки
field-capacity = Ёмкость
field-categories = Категории
field-chart = Chart
field-claim = Claim
field-class = Класс
field-cluster-ips = Cluster IP
field-completed = Завершён
field-completions = Завершения
field-concurrency-policy = Политика параллельности
field-container = Контейнер
field-container-runtime = Среда контейнеров
field-controller = Контроллер
field-conversion = Конвертация
field-conversion-webhook = Webhook конвертации
field-data-source = Источник данных
field-default = По умолчанию
field-default-backend = Бэкенд по умолчанию
field-delete = Удаление
field-deleting-objects-failed = Сбой удаления объектов
field-deletion-requested = Запрошено удаление
field-description = Описание
field-destination = Назначение
field-duration = Длительность
field-external-ip = Внешний IP
field-external-traffic-policy = Политика внешнего трафика
field-extras = Дополнительно
field-failure-policy = Политика при сбое
field-finalizers-remaining = Остались finalizers
field-finished = Завершено
field-from = Откуда
field-generators = Генераторы
field-global-default = Глобально по умолчанию
field-group = Группа
field-health = Состояние
field-health-message = Сообщение о состоянии
field-healthy-pods = Исправные Pod
field-history-failed = История (неуспешные)
field-history-successful = История (успешные)
field-host-ip = IP хоста
field-hostnames = Имена хостов
field-immutable = Неизменяемый
field-initiated-by = Инициатор
field-internal-traffic-policy = Политика внутреннего трафика
field-interval = Интервал
field-ip-families = Семейства IP
field-kernel = Ядро
field-kind = Kind
field-kubelet = Kubelet
field-last-applied-revision = Последняя применённая ревизия
field-last-attempted-revision = Последняя попытка ревизии
field-last-handled-reconcile = Последний обработанный reconcile
field-last-scaled = Последнее масштабирование
field-last-schedule = Последний запуск
field-last-successful = Последний успешный
field-limit = Лимит
field-lower = Нижняя граница
field-match-conditions = Условия совпадения
field-match-policy = Политика совпадения
field-max-replicas = Макс. реплик
field-max-unavailable = Макс. недоступных
field-message = Сообщение
field-min-available = Мин. доступных
field-min-replicas = Мин. реплик
field-misscheduled = Ошибочно размещены
field-mount-options = Параметры монтирования
field-name = Имя
field-names = Имена
field-namespace = Namespace
field-namespace-selector = Селектор Namespace
field-node = Node
field-node-selector = Node selector
field-object-selector = Селектор объектов
field-objects = Объекты
field-objects-remaining = Остались объекты
field-os-image = Образ ОС
field-parallelism = Параллельность
field-parameters = Параметры
field-parent = Родитель
field-path = Путь
field-paused = Приостановлен
field-phase = Фаза
field-pod-cidr = Pod CIDR
field-pod-ip = IP Pod
field-pod-ips = IP Pod
field-pod-management = Управление Pod
field-pod-selector = Селектор Pod
field-pods = Pod
field-policy-types = Типы политик
field-preemption = Вытеснение
field-priority = Приоритет
field-project = Проект
field-provider = Провайдер
field-provider-id = ID провайдера
field-provisioner = Provisioner
field-prune = Очистка
field-qos-class = Класс QoS
field-ready = Готовность
field-reason = Причина
field-reclaim-policy = Политика освобождения
field-recommenders = Рекомендатели
field-reconciled-at = Reconcile выполнен
field-ref = Ref
field-reference = Ссылка
field-reinvocation = Повторный вызов
field-release-name = Имя release
field-replicas = Реплики
field-repo = Репозиторий
field-request = Запрос
field-resource = Ресурс
field-resource-names = Имена ресурсов
field-resources = Ресурсы
field-restart-policy = Политика перезапуска
field-retries = Повторы
field-review-versions = Версии review
field-revision = Ревизия
field-roles = Роли
field-rule = Правило
field-rules = Правила
field-scale-down = Уменьшение
field-scale-up = Увеличение
field-schedule = Расписание
field-schema = Схема
field-scope = Область
field-scope-selector = Селектор области
field-scopes = Области
field-secret = Secret
field-selector = Селектор
field-served = Обслуживается
field-service = Service
field-service-account = Service account
field-serviceaccount = ServiceAccount
field-session-affinity = Привязка сессии
field-short-names = Короткие имена
field-side-effects = Побочные эффекты
field-source = Источник
field-started = Запущен
field-starting-deadline = Срок запуска
field-status = Статус
field-storage = Хранение
field-storage-class = Storage class
field-stored-versions = Хранимые версии
field-strategy = Стратегия
field-subjects = Субъекты
field-suspend = Приостановка
field-suspended = Приостановлен
field-sync-status = Статус синхронизации
field-synced-revision = Синхронизированная ревизия
field-target = Цель
field-target-namespace = Целевой Namespace
field-target-revision = Целевая ревизия
field-template-name = Имя шаблона
field-timeout = Тайм-аут
field-timezone = Часовой пояс
field-to = Куда
field-tolerations = Tolerations
field-total = Всего
field-type = Тип
field-types = Типы
field-uncapped = Без ограничений
field-unhealthy-pod-eviction = Вытеснение неисправных Pod
field-unschedulable = Планирование запрещено
field-update-mode = Режим обновления
field-update-strategy = Стратегия обновления
field-upper = Верхняя граница
field-url = URL
field-value = Значение
field-verbs = Глаголы
field-version = Версия
field-volume = Том
field-volume-expansion = Расширение тома
field-volume-mode = Режим тома
field-webhooks = Webhooks

## Values of detail fields that are texts of ours (by their English text)

value-yes = Да
value-no = Нет
value-not-allowed-kubernetes-keeps-this-namespace = Запрещено: Kubernetes хранит этот Namespace
value-system-namespace-deleting-it-asks-to-type-its-name = Системный Namespace: для удаления нужно ввести его имя
value-deleting-the-objects-inside-nothing-reported-as-stuck-yet = Удаляются объекты внутри; пока ничего не застряло
value-no-rule-all-ingress-is-denied = Нет правил: весь входящий трафик запрещён
value-no-rule-all-egress-is-denied = Нет правил: весь исходящий трафик запрещён
value-all-pods = все Pod
value-all-namespaces = все Namespace

