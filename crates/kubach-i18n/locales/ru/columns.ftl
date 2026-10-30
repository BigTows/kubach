# Тексты встроенных плагинов ресурсов kubach-core в интерфейсе: заголовки
# колонок, названия действий, секции деталей и состояния. В core они
# английские (их читает агент MCP), приложение переводит их по id.
# Термины Kubernetes (Namespace, Node, Kind, Role, Ingress, Chart…) не
# переводятся.


## Заголовки колонок

column-access-modes = Режимы доступа
column-active = Активные
column-address = Адрес
column-addresses = Адреса
column-age = Возраст
column-allowed = Допустимо прерываний
column-applications = Приложения
column-available = Доступно
column-backends = Бэкенды
column-binding-mode = Режим привязки
column-capacity = Ёмкость
column-chart = Chart
column-claim = Claim
column-class = Класс
column-cluster-ip = Cluster IP
column-completions = Завершения
column-containers = Контейнеры
column-controller = Контроллер
column-cpu = CPU
column-current = Текущее
column-data = Данные
column-default = По умолчанию
column-desired = Желаемое
column-destination = Назначение
column-duration = Длительность
column-egress = Egress
column-expansion = Расширение
column-external-ip = Внешний IP
column-failure-policy = Политика при сбое
column-from = Откуда
column-generators = Генераторы
column-global-default = Глобально по умолчанию
column-group = Группа
column-health = Состояние
column-healthy = Исправные
column-hostnames = Имена хостов
column-hosts = Хосты
column-images = Образы
column-ingress = Ingress
column-internal-ip = Внутренний IP
column-ip = IP
column-kind = Kind
column-labels = Метки
column-last-schedule = Последний запуск
column-listeners = Listeners
column-max = Макс.
column-max-unavailable = Макс. недоступных
column-memory = Память
column-min = Мин.
column-min-available = Мин. доступных
column-mode = Режим
column-most-used = Больше всего занято
column-name = Имя
column-namespace = Namespace
column-node = Node
column-node-selector = Node selector
column-parameters = Параметры
column-parents = Родители
column-path = Путь
column-pod-selector = Селектор Pod
column-policy-types = Типы политик
column-ports = Порты
column-preemption = Вытеснение
column-project = Проект
column-provisioner = Provisioner
column-ready = Готово
column-reclaim-policy = Политика освобождения
column-ref = Ref
column-reference = Ссылка
column-replicas = Реплики
column-repo = Репозиторий
column-request = Запрос
column-resources = Ресурсы
column-restarts = Перезапуски
column-revision = Ревизия
column-role = Role
column-roles = Roles
column-rules = Правила
column-runtime = Среда контейнеров
column-schedule = Расписание
column-scope = Область
column-selector = Селектор
column-source = Источник
column-status = Статус
column-storage-class = Storage class
column-subjects = Субъекты
column-suspend = Приостановка
column-suspended = Приостановлен
column-sync = Синхронизация
column-target = Цель
column-targets = Цели
column-timezone = Часовой пояс
column-to = Куда
column-type = Тип
column-types = Типы
column-up-to-date = Актуальные
column-url = URL
column-value = Значение
column-version = Версия
column-versions = Версии
column-volume = Том
column-webhooks = Webhooks

## Действия

action-delete = Удалить
action-scale = Масштабировать
action-restart = Перезапустить
action-trigger = Запустить сейчас
action-suspend = Приостановить
action-resume = Возобновить
action-cordon = Cordon
action-uncordon = Uncordon
action-evict = Вытеснить
action-logs = Логи
action-exec = Exec
action-port-forward = Переадресация порта
action-debug = Отладка
action-refresh = Обновить
action-hard-refresh = Полное обновление
action-sync = Синхронизировать
action-reconcile = Согласовать

## Секции деталей

section-active-jobs = Активные Job
section-addresses = Адреса
section-allocatable = Доступно для выделения
section-allowed-topologies = Допустимые топологии
section-annotations = Аннотации
section-applications = Приложения
section-backends = Бэкенды
section-behavior = Поведение
section-binary-data = Двоичные данные
section-capacity = Ёмкость
section-conditions = Условия
section-container-policies = Политики контейнеров
section-containers = Контейнеры
section-data = Данные
section-deletion-blocked-by = Удаление блокируют
section-diagnostics = Диагностика
section-disrupted-pods = Прерванные Pod
section-egress-rules = Правила egress
section-finalizers = Finalizers
section-from = Откуда
section-history = История
section-images = Образы
section-ingress-rules = Правила ingress
section-init-containers = Init-контейнеры
section-inventory = Инвентарь
section-labels = Метки
section-listeners = Listeners
section-metrics = Метрики
section-operation = Операция
section-owner-references = Владельцы
section-parameters = Параметры
section-ports = Порты
section-probes = Проверки
section-recommendations = Рекомендации
section-resources = Ресурсы
section-resources-summary = Сводка ресурсов
section-rules = Правила
section-rules-aggregated = Правила (агрегированные)
section-scheduling = Планирование
section-security = Безопасность
section-selector = Селектор
section-source = Источник
section-sources = Источники
section-subjects = Субъекты
section-sync-history = История синхронизаций
section-sync-result = Результат синхронизации
section-system-info = Сведения о системе
section-taints = Taints
section-template-sources = Источники шаблонов
section-to = Куда
section-usage = Использование
section-used-by-pods = Используется в Pod
section-versions = Версии
section-volume-claim-templates = Шаблоны volume claim
section-volumes = Тома

## Состояния

health-healthy = Исправен
health-progressing = Выполняется
health-degraded = Деградирован
health-failed = Сбой
health-unknown = Неизвестно

## Заголовки секций с данными

section-limits = Лимиты: { $item }
section-parent = Родитель { $parent }
