# Экран GitOps: Argo CD Applications, Flux Kustomizations, HelmReleases и
# источники. Состояния синхронизации и здоровья (Synced, OutOfSync, Healthy),
# kind, имена, ревизии и ошибки — данные, не переводятся.

## Уровни внимания (колонка статуса и бейдж)

gitops-attention-ok = OK
gitops-attention-unknown = Неизвестно
gitops-attention-suspended = Приостановлен
gitops-attention-pending = В процессе
gitops-attention-warning = Предупреждение
gitops-attention-error = Ошибка

## Список

gitops-problems-only = Только проблемы
gitops-objects =
    { $count ->
        [one] { $count } объект
        [few] { $count } объекта
        [many] { $count } объектов
       *[other] { $count } объекта
    }
gitops-count-failing = с ошибкой
gitops-count-out-of-sync = не синхронизировано
gitops-count-in-progress = в процессе
gitops-count-suspended = приостановлено
gitops-count-healthy = в порядке
gitops-list-error = { $kind }: { $error }
gitops-list-error-in = { $kind } в { $namespace }: { $error }
gitops-cant-list-kinds = Не удаётся получить список: { $errors }
gitops-noun-problems = Проблемы
gitops-noun-objects = Объекты GitOps
gitops-no-problems = Проблем нет
gitops-no-problems-description = Всё синхронизировано, согласовано или в процессе.

## Нечего показать

gitops-no-tools = Нет инструментов GitOps
gitops-no-tools-description = В этом кластере нет ни Argo CD, ни Flux.
gitops-cant-list = Не удаётся получить объекты GitOps
gitops-empty = Нет Applications, Kustomizations и HelmReleases
gitops-empty-any-namespace = Ни в одном namespace ничего нет.
gitops-empty-selected-namespaces = В выбранных namespace ничего нет. Argo CD Applications обычно лежат в argocd, объекты Flux — в flux-system.

## Детали

gitops-cluster-scoped = { $kind } · кластер
gitops-badge-sync = Синхронизация: { $state }
gitops-badge-ready = Готовность: { $state }
gitops-badge-health = Здоровье: { $state }
gitops-errors = Ошибки
gitops-overview = Обзор
gitops-revision = Ревизия
gitops-attempted-revision = Ревизия последней попытки
gitops-destination = Назначение
gitops-ref = Ref
gitops-target-namespace = Целевой namespace
gitops-url = URL
gitops-source = Источник
gitops-source-object = Объект источника
gitops-status-changed = Статус изменён
gitops-suspended = Приостановлен
gitops-managed = Управляемые ресурсы · { $count }
