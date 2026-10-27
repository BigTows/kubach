# Действия над объектами: подтверждения, тосты запусков, масштабирование
# до 0, Drain и Shell для Node. Имена, kind, флаги и команды kubectl — данные.

## Подтверждение действия

action-confirm-title-one = { $action } { $kind } { $name }?
action-confirm-title-many = { $action } { $count } { $title }?
action-confirm-label-many = { $action } { $count }
action-confirm-one = { $action } { $object }?
action-confirm-many =
    { $scope ->
        [any] { $action } { $count } { $title } во всех namespace?
        [one] { $action } { $count } { $title } в namespace { $namespace }?
        [many] { $action } { $count } { $title } в { $namespaces } namespace?
       *[cluster] { $action } { $count } { $title }?
    }
action-delete-one = { $object } будет удалён безвозвратно.
action-delete-many =
    { $scope ->
        [any] Будут безвозвратно удалены { $title } во всех namespace: { $count }.
        [one] Будут безвозвратно удалены { $title } в namespace { $namespace }: { $count }.
        [many] Будут безвозвратно удалены { $title } в { $namespaces } namespace: { $count }.
       *[cluster] Будут безвозвратно удалены { $title }: { $count }.
    }
action-evict-one = Pod { $object } будет вытеснен через Eviction API (с учётом PodDisruptionBudget).
action-evict-many = Pod будут вытеснены через Eviction API (с учётом PodDisruptionBudget): { $count }.
action-trigger-one = Создаёт Job из CronJob { $object } сейчас, вне расписания.
action-trigger-many = Создаёт по Job из каждого CronJob сейчас, вне расписания: { $count }.
action-cordon-many = На эти ноды ({ $count }) не будут назначаться новые Pod, пока не будет выполнен uncordon.
action-claim-used-by = Используется в Pod { $pods }: claim останется в Terminating ({ $finalizer }), пока они не будут удалены.
action-claim-of = { $object }: { $note }
action-claim-maybe-used = Claim, который ещё смонтирован в Pod, останется в Terminating ({ $finalizer }), пока Pod не будет удалён.
action-namespace-holds = В { $namespace }: { $contents }. Удаление namespace удаляет все объекты в нём, и это нельзя отменить.
action-namespace-empty = В { $namespace } нет объектов основных kind (Pod, рабочие нагрузки, сервисы, конфигурация, claim). Удаление namespace удаляет все объекты в нём, и это нельзя отменить.
action-namespace-unknown = Всё содержимое { $namespace } будет удалено вместе с ним. Удаление namespace удаляет все объекты в нём, и это нельзя отменить.
action-namespace-count =
    { $count ->
        [1] 1 { $kind }
       *[other] { $count } { $title }
    }

## Запуск действия (тост)

action-run-title =
    { $count ->
        [1] { $action } 1 { $kind }
       *[other] { $action } { $count } { $title }
    }
action-run-aborted = Массовое действие прервано: { $error }
action-rbac-hint = Kubernetes RBAC отказал для части объектов (HTTP 403): у ваших учётных данных там нет этого права. Проверьте через kubectl auth can-i или обратитесь к администратору кластера.
action-error-forbidden = Нет прав (RBAC): { $error }
action-error-not-found = Не найден: { $error }
action-error-text = { $error }

## Масштабирование до 0

action-scale-zero-title = Масштабировать { $kind } { $name } до 0?
action-scale-zero-message = Все Pod { $object } будут остановлены; он ничего не будет обслуживать, пока его снова не масштабируют.
action-scale-zero-confirm = Масштабировать до 0

## Drain для Node

drain-confirm-title = Drain для Node { $node }?
drain-confirm-message =
    { $minutes ->
        [one] Выполняет cordon для { $node }, затем вытесняет его Pod через Eviction API: PodDisruptionBudget соблюдаются, а их отказы повторяются до { $minutes } минуты. Pod DaemonSet и статические Pod остаются.
        [few] Выполняет cordon для { $node }, затем вытесняет его Pod через Eviction API: PodDisruptionBudget соблюдаются, а их отказы повторяются до { $minutes } минут. Pod DaemonSet и статические Pod остаются.
        [many] Выполняет cordon для { $node }, затем вытесняет его Pod через Eviction API: PodDisruptionBudget соблюдаются, а их отказы повторяются до { $minutes } минут. Pod DaemonSet и статические Pod остаются.
       *[other] Выполняет cordon для { $node }, затем вытесняет его Pod через Eviction API: PodDisruptionBudget соблюдаются, а их отказы повторяются до { $minutes } минуты. Pod DaemonSet и статические Pod остаются.
    }
drain-list-failed = Не удалось получить список Pod ноды ({ $error }); drain запросит его снова при запуске.
drain-confirm = Drain
drain-option-ignore-daemonsets = Игнорировать DaemonSet (--ignore-daemonsets)
drain-option-delete-emptydir = Удалять данные emptyDir (--delete-emptydir-data)
drain-option-force = Принудительно: Pod без контроллера (--force)
drain-blocked-notice = Эти Pod останавливают drain; отметьте нужные им опции:
drain-emptydir-notice = Данные в томах emptyDir вытесненных Pod будут потеряны.
drain-plan-line = { $pod } — { $what }
drain-verdict-evict = вытеснить
drain-verdict-delete = удалить (завершён)
drain-verdict-wait = уже завершается
drain-verdict-skip = пропустить: { $reason }
drain-verdict-blocked = блокирует: { $reason }
drain-skip-mirror = статический (mirror) Pod
drain-skip-daemonset = Pod DaemonSet
drain-block-daemonset = Pod DaemonSet: отметьте «Игнорировать DaemonSet»
drain-block-no-controller = не управляется контроллером: отметьте «Принудительно»
drain-block-emptydir = использует emptyDir (данные будут потеряны): отметьте «Удалять данные emptyDir»
drain-left-on-node = Осталось на ноде: { $pods }.
drain-skipped-mirror =
    { $count ->
        [one] { $count } статический (mirror) Pod
       *[other] { $count } статических (mirror) Pod
    }
drain-skipped-daemonset = Pod DaemonSet: { $count }
drain-cordon-failed = Cordon не удался: { $error }
drain-list-pods-failed = Не удалось получить список Pod: { $error }
drain-cancelled-evicting = Отменено во время вытеснения
drain-eviction-timeout = Вытеснение осталось без ответа к истечению тайм-аута drain
drain-gave-up = { $error }; попытки прекращены по тайм-ауту drain
drain-cancelled = Отменено: { $error }
drain-terminating-timeout = Вытеснен, но ещё завершался к истечению тайм-аута drain
drain-cancelled-waiting = Отменено во время ожидания завершения Pod

## Shell на Node

node-shell-confirm-title = Shell на Node { $node }?
node-shell-confirm-message = Kubach создаёт привилегированный Pod на { $node } в namespace { $namespace } (PID, сеть и IPC хоста, допускает любые taint) и открывает root-shell на хосте через nsenter. Pod удаляется при закрытии вкладки. Namespace и образ задаются в Настройки → Общие → Терминал.
node-shell-image = Образ: { $image }
node-shell-root-notice = Shell работает от root на ноде: он может читать данные любого Pod и изменять хост.
node-shell-confirm = Открыть shell
