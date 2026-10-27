# Браузер ресурсов: список kind с панелью фильтров, панель деталей, блоки
# Pods и Connections, выбор namespace. Kind во множественном числе ($title:
# «Deployments», «Pods»), имена, namespace и ошибки кластера — данные.
# Термины Kubernetes (Pod, Node, namespace, kind, HPA, PodDisruptionBudget,
# Eviction API, emptyDir) не переводятся.

## Список kind

browser-resources = Ресурсы
browser-place-object = { $kind } { $name }
browser-search-placeholder = Поиск по имени…
browser-list-count =
    { $scope ->
        [any] { $title }: { $count } во всех namespace
        [one] { $title }: { $count } в namespace { $namespace }
        [many] { $title }: { $count } в { $namespaces } namespace
       *[cluster] { $title }: { $count }
    }
browser-shown-of = { $shown } из { $total }
browser-cached-ago = из кэша, { $age } назад

## Нечего показать

browser-no-cluster = Кластер не подключён
browser-connecting = Подключение…
browser-not-connected = Нет подключения
browser-discovering = Поиск ресурсов API…
browser-discovery-failed = Не удалось получить ресурсы API
browser-not-served = Не поддерживается
browser-not-served-description = Этот кластер не поддерживает этот kind. Выберите другой в боковой панели.
browser-loading = Загрузка { $title }…
browser-no-access = Нет доступа к { $title }
browser-no-access-description =
    { $scope ->
        [any] Ваши учётные данные не позволяют получить список { $title } во всех namespace. { $error }
        [one] Ваши учётные данные не позволяют получить список { $title } в namespace { $namespace }. { $error }
        [many] Ваши учётные данные не позволяют получить список { $title } в { $namespaces } namespace. { $error }
       *[cluster] Ваши учётные данные не позволяют получить список { $title }. { $error }
    }
browser-cant-load = Не удаётся загрузить { $title }
browser-lost-connection-retrying = Потеряно соединение с кластером. Повтор… ({ $error })
browser-error-retrying = { $error }. Повтор…
browser-empty = Нет { $title }
browser-nothing-found =
    { $scope ->
        [any] Ничего не найдено во всех namespace.
        [one] Ничего не найдено в namespace { $namespace }.
        [many] Ничего не найдено в { $namespaces } namespace.
       *[cluster] Ничего не найдено.
    }

## Баннеры над таблицей

browser-banner-connection-lost = Потеряно соединение с кластером, показаны данные из кэша. Повтор…
browser-banner-cant-refresh = Не удаётся обновить { $title }, показаны данные из кэша. Повтор…
browser-banner-no-access = Нет доступа к { $title } в { $namespaces }; показаны остальные namespace.
browser-banner-cant-load = Не удаётся загрузить { $title } в { $namespaces }. Повтор…
browser-all-namespaces = все namespace
browser-offline-error = Нет подключения, показан последний список из кэша: { $error }
browser-offline-disconnected = Отключено, показан последний список из кэша.
browser-offline-connecting = Пока идёт подключение, показан последний список из кэша…
browser-pruned =
    { $count ->
        [one] Namespace { $names } больше нет; он убран из выбора.
        [few] Namespace { $names } больше нет; они убраны из выбора.
        [many] Namespace { $names } больше нет; они убраны из выбора.
       *[other] Namespace { $names } больше нет; они убраны из выбора.
    }

## Действия с выбранными

browser-selected = Выбрано: { $count }
browser-no-common-action = Нет действий, применимых ко всем выбранным объектам
browser-clear-selection = Снять выделение

## Выбор namespace

picker-all-namespaces = Все namespace
picker-cluster-scoped = Уровня кластера
picker-no-permission = Нет прав на список namespace; введите имя.
picker-unavailable = Namespace недоступны; введите имя.

## Панель деталей

details-ago = { $age } назад
details-back-tooltip = Назад: { $place } (Esc)
details-evict-tooltip = Вытеснить: через Eviction API с учётом PodDisruptionBudget
details-drain-tooltip = Drain…: закрыть Node для планирования и вытеснить его Pod
details-node-shell-tooltip = Оболочка Node: привилегированный Pod с оболочкой на хосте
details-scale-down = Уменьшить на одну реплику
details-scale-up = Увеличить на одну реплику
details-replicas =
    { $count ->
        [one] { $count } реплика
        [few] { $count } реплики
        [many] { $count } реплик
       *[other] { $count } реплики
    }
details-scale-hpa = Управляется HPA { $name }: число реплик будет перезаписано
details-scale-offline = Нет подключения: число реплик взято из кэша
details-scale-loading = Загрузка текущего числа реплик…
details-scale-reconnecting = Переподключение: число реплик может быть устаревшим
details-show-all = Показать все объекты { $kind }
details-open = Открыть { $kind } { $name }
details-name = Имя
details-namespace = Namespace
details-created = Создан
details-uid = UID
details-copy-uid = Скопировать UID
details-reason = Причина
details-security = Безопасность
details-owner = Владелец
details-managed-by = Управляется
details-labels = Метки
details-annotations = Аннотации
details-loading = Загрузка…
details-unavailable = Недоступно: { $error }
details-none = Нет
details-not-created-yet = ещё не создан
details-not-served = кластер не поддерживает { $kind }
details-not-found = Не найден: объекта больше нет.
details-no-access = Нет доступа к объектам этого kind здесь.
details-cant-load = Не удаётся загрузить: { $error }

## Контейнеры в деталях

details-container-ready = готов
details-container-not-ready = не готов
details-container-restarts =
    { $count ->
        [one] { $count } перезапуск
        [few] { $count } перезапуска
        [many] { $count } перезапусков
       *[other] { $count } перезапуска
    }
details-image = Образ
details-state = Состояние
details-last-state = Прошлое состояние
details-ports = Порты
details-requests = Запросы
details-limits = Лимиты
details-environment = Переменные окружения ({ $count })
details-mounts = Монтирования ({ $count })

## Порты контейнеров

details-port-tcp-only = { $protocol }: переадресация портов поддерживает только TCP
details-port-forwarded-open = Переадресован на { $address }. Нажмите, чтобы открыть { $url }
details-port-forwarded-copy = Переадресован на { $address }. Нажмите, чтобы скопировать адрес
details-port-forward = Переадресовать { $port }: выбрать локальный порт
details-port-open = Открыть { $url }
details-copy-address = Скопировать адрес
details-stop-forward = Остановить переадресацию

## Запуски из деталей

details-run-scale-down = Уменьшение { $name }
details-run-scale-up = Увеличение { $name }
details-action-interrupted = Действие прервано
node-drain-run = Drain { $node }
node-drain-cordoning = Cordon и получение списка Pod…
node-drain-no-pods = Нет Pod для вытеснения
node-drain-finishing = Завершение…
node-drain-evicting =
    { $count ->
        [one] Вытеснение: остался { $count } Pod
        [few] Вытеснение: осталось { $count } Pod
        [many] Вытеснение: осталось { $count } Pod
       *[other] Вытеснение: осталось { $count } Pod
    }
node-drain-aborted = Drain прерван: { $error }

## Pod рабочей нагрузки

pods-none = Нет Pod
pods-not-scheduled = Не назначен

## Содержимое namespace

namespace-contents = Содержимое
namespace-counting = Подсчёт…
namespace-no-permission = нет прав
namespace-not-counted = не подсчитано: { $error }
namespace-contents-empty = Нет Pod, рабочих нагрузок, сервисов, конфигурации и claim
namespace-count-again = Пересчитать

## Блок «Управляется»

managed-by-missing = не найден в этом кластере
managed-by-not-served = kind не установлен в этом кластере
managed-by-failed = не удаётся проверить: { $error }
managed-by-open = Открыть · по { $via }
managed-by-note = { $note } · по { $via }

## Блок Connections

connections-title = Связи
connections-none = Нет связей с другими объектами
