# Kustomize: сборка папки kustomization, объекты сборки и сравнение с
# кластером. Пути, kind, имена объектов, версии API и вывод kustomize и
# kubectl — данные. Термины Kubernetes (kind, namespace, Secret,
# kustomization, helmCharts) не переводятся.

kustomize-description = Соберите папку kustomization и сравните её с кластером.
kustomize-recent = Недавние
kustomize-recent-tooltip = Недавние папки
kustomize-enable-helm-tooltip = Разворачивать helmCharts (нужен helm в PATH)
kustomize-rebuild-tooltip = Собрать снова
kustomize-open-folder = Открыть папку…
kustomize-no-folder = Папка не открыта
kustomize-no-folder-description = Выберите папку с kustomization.yaml. Kubach локально запускает `kustomize build` (или `kubectl kustomize`); ничего не применяется.
kustomize-building = Сборка…

## Ошибки сборки

kustomize-error-no-tool = kustomize недоступен
kustomize-error-no-tool-hint = Установите kustomize или kubectl и проверьте, что он есть в PATH (Kubach читает PATH из вашей login-оболочки).
kustomize-error-no-tool-details = в PATH нет ни `kustomize`, ни `kubectl`
kustomize-error-not-kustomization = Это не папка kustomization
kustomize-error-not-kustomization-hint = Выберите папку, в которой есть kustomization.yaml, kustomization.yml или Kustomization.
kustomize-error-not-kustomization-details = В { $path } нет kustomization.yaml
kustomize-error-spawn = Не удалось запустить kustomize
kustomize-error-spawn-hint = Проверьте, что бинарь существует и исполняемый.
kustomize-error-spawn-details = не удалось запустить { $program }: { $error }
kustomize-error-timeout = Время сборки истекло
kustomize-error-timeout-hint = Удалённым базам и helmCharts нужен доступ к сети; проверьте URL.
kustomize-error-timeout-details = kustomize build не завершился за { $seconds } с
kustomize-error-failed = Сборка не удалась
kustomize-error-unreadable = Вывод сборки не читается
kustomize-error-not-utf8 = вывод kustomize не в UTF-8
kustomize-error-yaml = некорректный вывод kustomize: { $error }
kustomize-error-interrupted = Сборка прервана

## Объекты

kustomize-column-kind = Kind
kustomize-column-name = Имя
kustomize-column-namespace = Namespace
kustomize-column-api-version = Версия API
kustomize-column-cluster = В кластере
kustomize-status-new = Новый
kustomize-status-unchanged = Без изменений
kustomize-status-changed = Изменён
kustomize-status-error = Ошибка
kustomize-tab-resources = Ресурсы
kustomize-tab-output = Вывод
kustomize-tab-diff = Сравнение с кластером
kustomize-objects =
    { $count ->
        [one] { $count } объект
        [few] { $count } объекта
        [many] { $count } объектов
       *[other] { $count } объекта
    }
kustomize-compare-connect = подключитесь к кластеру, чтобы сравнить
kustomize-compare-running = сравнение с кластером…
kustomize-compare-result = кластер: { $counts }
kustomize-built-with = собрано: { $tool }
kustomize-count-new = новых: { $count }
kustomize-count-changed = изменённых: { $count }
kustomize-count-unchanged = без изменений: { $count }
kustomize-count-failed = не сравнено: { $count }
kustomize-count-nothing = нечего сравнивать
kustomize-empty-build = Пустая сборка
kustomize-empty-build-description = Kustomization не создала ни одного объекта.
kustomize-secrets-hidden = Значения секретов скрыты
kustomize-secrets-hidden-tooltip = Значения секретов скрыты настройками безопасности этого контекста.
kustomize-secrets-hide = Скрыть значения секретов
kustomize-secrets-reveal = Показать значения секретов

## Сравнение с кластером

kustomize-compare-error = Не удалось сравнить
kustomize-comparing = Сравнение с кластером…
kustomize-no-cluster = Нет подключения к кластеру
kustomize-no-cluster-description = Подключитесь к кластеру, чтобы сравнить с ним сборку.
kustomize-no-differences = Различий нет
kustomize-no-differences-description = В живом объекте уже есть все поля, которые задаёт сборка.
kustomize-diff-new = В кластере ещё нет: применение создаст объект.
kustomize-diff-fields =
    { $count ->
        [one] Отличается { $count } поле. Сравниваются только поля, заданные сборкой; значения сервера по умолчанию и status не учитываются.
        [few] Отличаются { $count } поля. Сравниваются только поля, заданные сборкой; значения сервера по умолчанию и status не учитываются.
        [many] Отличаются { $count } полей. Сравниваются только поля, заданные сборкой; значения сервера по умолчанию и status не учитываются.
       *[other] Отличаются { $count } поля. Сравниваются только поля, заданные сборкой; значения сервера по умолчанию и status не учитываются.
    }
