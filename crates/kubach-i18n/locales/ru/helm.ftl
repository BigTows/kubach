# Helm: страницы «Релизы» и «Чарты», детали релиза, откат, удаление,
# обновление и установка, версии OCI-чартов, Настройки → Внешние
# инструменты (бинарь helm и копия, которую скачивает Kubach) и отказ
# изменения, запрошенного в другом контексте. Имена релизов, чартов,
# репозиториев, namespace и контекстов, версии, статусы (deployed,
# pending-upgrade), команды helm и его вывод — данные. Термины Kubernetes и
# Helm (Helm, release, chart, values, manifest, hook, Secret, ConfigMap, PVC,
# PersistentVolumeClaim, CRD, OCI) в коде и командах не переводятся; в
# интерфейсе «релиз», «чарт», «values», «манифест», «хук».

## Страницы

helm-page-releases = Релизы Helm
helm-page-charts = Чарты Helm
helm-no-cluster = Нет подключения к кластеру
helm-connecting = Подключение…
helm-not-connected = Не подключено
helm-loading = Загрузка…
helm-cancel = Отмена
helm-back = Назад
helm-dismiss = Скрыть

## Список релизов

helm-reload-releases = Перезагрузить релизы
helm-reloading-releases = Перезагрузка релизов…
helm-search-releases = Поиск релизов по имени…
helm-noun-releases = Релизы
helm-driver-detected = хранятся в объектах { $kind }
helm-driver-settings = хранятся в объектах { $kind } (по настройкам)
helm-driver-default = ищутся в объектах { $kind }
helm-releases-scope =
    { $scope ->
        [all] во всех namespace
        [one] в namespace { $namespace }
       *[many] в { $namespaces } namespace
    }
helm-releases-loading = Загрузка релизов Helm…
helm-releases-list-error = Не удалось получить список релизов Helm
helm-all-namespaces = все namespace
helm-releases-forbidden = Нет доступа к релизам Helm ({ $kind }) в { $namespaces }: ваши учётные данные не позволяют получать там список { $kind }.
helm-releases-list-failed = Не удалось получить релизы в { $namespaces }: { $error }
helm-releases-none = Нет релизов Helm
helm-releases-none-description = Ничего не найдено { $scope } ({ $driver }). Драйвер хранилища можно задать для контекста в Настройки → Кластеры.
helm-releases-none-shown = Релизы Helm не показаны
helm-releases-summary =
    { $count ->
        [one] { $count } релиз { $scope } · { $driver }
        [few] { $count } релиза { $scope } · { $driver }
        [many] { $count } релизов { $scope } · { $driver }
       *[other] { $count } релиза { $scope } · { $driver }
    }
helm-releases-summary-filtered =
    { $count ->
        [one] { $shown } из { $count } релиза { $scope } · { $driver }
       *[other] { $shown } из { $count } релизов { $scope } · { $driver }
    }
helm-cluster-serves-no = кластер не предоставляет { $kind }
helm-listing-interrupted = Получение списка прервано: { $error }

## Детали релиза

helm-back-releases = Релизы
helm-release-loading = Загрузка релиза…
helm-release-load-error = Не удалось загрузить релиз
helm-release-not-found = Релиз не найден
helm-loading-interrupted = Загрузка прервана: { $error }
helm-revision = Ревизия { $revision }
helm-revision-latest = { $revision } (последняя)
helm-revision-of = { $revision } из { $latest }
helm-field-status = Статус
helm-field-revision = Ревизия
helm-field-chart = Чарт
helm-field-version = Версия
helm-field-app-version = Версия приложения
helm-field-updated = Обновлён
helm-field-first-deployed = Первое развёртывание
helm-field-description = Описание
helm-field-labels = Метки
helm-field-repository = Репозиторий
helm-field-home = Сайт
helm-field-sources = Исходники
helm-field-keywords = Ключевые слова
helm-field-maintainers = Сопровождающие
helm-field-created = Создан
helm-column-name = Имя
helm-column-namespace = Namespace
helm-showing-old-revision = Показана ревизия { $revision } — это не текущее состояние релиза.
helm-show-latest = Показать последнюю
helm-update-available = Новая версия { $version } в репозитории { $repository }
helm-update-available-app = Новая версия { $version } (приложение { $app }) в репозитории { $repository }
helm-upgrade-to-tooltip = Обновить до { $version } из { $repository }
helm-tab-values = Values
helm-tab-computed-values = Итоговые values
helm-tab-manifest = Манифест
helm-tab-hooks = Хуки
helm-tab-notes = Заметки
helm-tab-history = История
helm-tab-diff = Сравнение
helm-hidden-secrets = Скрыто: могут быть секреты
helm-hidden-secrets-description = В values, манифестах и хуках часто есть пароли и токены. Показывайте их, только когда никто не смотрит через плечо.
helm-secret-values-hidden = Значения секретов скрыты настройками безопасности этого контекста.
helm-reveal = Показать
helm-notes-none = Нет заметок
helm-notes-none-description = В чарте нет NOTES.txt.
helm-history-hint = Выберите ревизию, чтобы посмотреть её на других вкладках или откатиться к ней.
helm-values-warning = предупреждение: { $warning }
helm-hooks-none = У этой ревизии нет хуков.
helm-hook-header = { $kind } { $name } · { $events } · вес { $weight }
helm-hook-delete-policy = политика удаления: { $policies }
helm-hook-last-run = последний запуск: { $phase }

## Вкладка «Сравнение»

helm-diff-nothing = Нечего сравнивать
helm-diff-single-revision = У релиза одна ревизия.
helm-diff-from = с
helm-diff-to = по
helm-diff-none = Различий нет
helm-diff-identical = { $source }: ревизии { $from } и { $to } совпадают.

## Откат, обновление, удаление

helm-rollback-button = Откатить к { $revision }
helm-rollback-title = Откатить { $name } к ревизии { $revision }?
helm-rollback-message = Helm создаст ревизию { $next } релиза { $name } в { $namespace } с манифестом и values ревизии { $revision }; выполнятся хуки отката чарта.
helm-rollback-confirm = Откатить
helm-rollback-toast = Откат { $name } к ревизии { $revision }
helm-upgrade-button = Обновить…
helm-upgrade-tooltip = Изменить values, посмотреть изменения манифеста и обновить
helm-upgrade-title = Обновить { $name }?
helm-upgrade-confirm-message = Helm создаст новую ревизию { $name } в { $namespace }, как в предпросмотре.
helm-upgrade-confirm-message-moved = Релиз изменился после ревизии { $revision }; предпросмотр мог устареть. Helm создаст новую ревизию { $name } в { $namespace }, как в предпросмотре.
helm-upgrade-confirm = Обновить
helm-upgrade-toast = Обновление { $name }
helm-upgrade-release-moved = Релиз изменился после ревизии { $revision }; предпросмотр мог устареть.
helm-uninstall-button = Удалить…
helm-uninstall-title = Удалить { $name }?
helm-uninstall-keep-history = Сохранить историю (--keep-history): релиз останется в списке как удалённый, и его можно будет откатить
helm-uninstall-delete-claims =
    { $count ->
        [one] Также удалить этот PVC после удаления релиза (его данные будут потеряны)
       *[other] Также удалить эти PVC после удаления релиза (их данные будут потеряны)
    }
helm-uninstall-confirm = Удалить
helm-uninstall-toast = Удаление { $name }
helm-uninstall-delete-hooks =
    { $count ->
        [one] Сначала выполнится { $count } хук удаления чарта.
        [few] Сначала выполнятся { $count } хука удаления чарта.
        [many] Сначала выполнятся { $count } хуков удаления чарта.
       *[other] Сначала выполнятся { $count } хука удаления чарта.
    }
helm-uninstall-resources =
    { $count ->
        [one] Helm удалит релиз { $name } в { $namespace } и его { $count } ресурс:
        [few] Helm удалит релиз { $name } в { $namespace } и его { $count } ресурса:
        [many] Helm удалит релиз { $name } в { $namespace } и его { $count } ресурсов:
       *[other] Helm удалит релиз { $name } в { $namespace } и его { $count } ресурса:
    }
helm-uninstall-no-resources = Helm удалит релиз { $name } в { $namespace }; в его манифесте нет ресурсов.
helm-claims-stay =
    { $count ->
        [one] { $count } PersistentVolumeClaim останется: Helm не удаляет claims, созданные StatefulSet или сохранённые чартом, поэтому их тома и данные остаются.
       *[other] { $count } PersistentVolumeClaim останутся: Helm не удаляет claims, созданные StatefulSet или сохранённые чартом, поэтому их тома и данные остаются.
    }
helm-claims-check-failed = Не удалось проверить, какие PersistentVolumeClaim останутся после удаления: { $error }
helm-lookup-interrupted = проверка прервана: { $error }
helm-delete-claim-toast = Удаление PVC { $name }
helm-delete-claims-toast = Удаление PVC: { $count }
helm-delete-claims-detail = В { $namespace }: { $names }
helm-claims-not-deleted = PVC не удалены: { $reason }
helm-claims-not-deleted-count = Не удалено PVC: { $count }
helm-delete-interrupted = удаление прервано: { $error }
helm-stop-install-title = Остановить установку { $release }?
helm-stop-upgrade-title = Остановить обновление { $release }?
helm-stop-message = Helm попросят перестать ждать, и он пометит релиз как failed; уже созданные ресурсы останутся, а запущенный Job хука продолжит работу. Если helm не завершится за { $seconds } с, он будет принудительно остановлен, и релиз может остаться в статусе { $status }, пока его не откатят или не удалят.
helm-call-interrupted = вызов helm прерван: { $error }
helm-installed = { $name } установлен в { $namespace }
helm-output-show = Показать вывод helm
helm-output-hide = Скрыть вывод helm

## Изменение, запрошенное в другом контексте, не выполняется

helm-context-changed = Контекст сменился на { $context }; действие отменено
helm-context-reconnected = Повторное подключение к { $context }; действие отменено
helm-context-disconnected = Отключено от { $context }; действие отменено

## Страница «Чарты»

helm-search-charts = Поиск чартов…
helm-no-home = Не удалось найти конфигурацию Helm: нет домашней папки.
helm-repositories = Репозитории
helm-repo-charts =
    { $count ->
        [one] { $count } чарт
        [few] { $count } чарта
        [many] { $count } чартов
       *[other] { $count } чарта
    }
helm-repo-index-missing = Не скачан: обновите его
helm-repo-index-invalid = Индекс не читается: { $error }
helm-repo-show-all = Показать чарты всех репозиториев
helm-repo-show-only = Показать только чарты этого репозитория
helm-repo-update-tooltip = Обновить индекс (helm repo update)
helm-repo-remove-tooltip = Удалить репозиторий (helm repo remove)
helm-repo-update-all-tooltip = Обновить все индексы (helm repo update)
helm-repo-add-tooltip = Добавить репозиторий (helm repo add)
helm-repo-reading = Чтение репозиториев…
helm-repo-none-yet = Репозиториев пока нет. Добавьте репозиторий по URL.
helm-repo-name-placeholder = имя, например prometheus-community
helm-repo-username-placeholder = имя пользователя (необязательно)
helm-repo-password-placeholder = пароль (необязательно)
helm-repo-private-note = Приватный репозиторий: helm хранит логин в своём файле репозиториев, пароль передаётся через stdin (--password-stdin). Учётные данные отправляются только в репозитории https://.
helm-repo-add = Добавить
helm-repo-exists = Репозиторий { $name } уже есть.
helm-repo-username-missing = Введите имя пользователя для пароля.
helm-repo-config-from = Из { $path }
helm-repo-adding = Добавление репозитория { $name }
helm-repo-updating-all = Обновление репозиториев чартов
helm-repo-updating = Обновление репозитория { $name }
helm-repo-updating-count =
    { $count ->
        [one] Обновление { $count } репозитория
       *[other] Обновление { $count } репозиториев
    }
helm-repo-removing = Удаление репозитория { $name }
helm-repo-remove-title = Удалить репозиторий { $name }?
helm-repo-remove-message = { $name } будет удалён из ваших репозиториев Helm (и для helm в терминале), его кешированный индекс — тоже. Установленные релизы не затрагиваются.
helm-repo-remove-confirm = Удалить
helm-charts-reading = Чтение репозиториев чартов…
helm-charts-no-repositories = Нет репозиториев чартов
helm-charts-no-repositories-description = Добавьте репозиторий по URL: Kubach запускает helm repo add, поэтому это ваш список Helm, общий с терминалом. Чарты из OCI-реестра или по URL ставятся без репозитория: «Установить из OCI или URL…».
helm-charts-no-indexes = Индексы не скачаны
helm-charts-no-indexes-description = Обновите репозитории (helm repo update), чтобы искать в их чартах.
helm-charts-repo-no-index = У { $name } нет скачанного индекса: обновите его, чтобы увидеть чарты.
helm-charts-repo-no-match = В { $name } ничего не найдено. Сбросьте фильтр репозитория, чтобы искать во всех.
helm-charts-no-match = В кешированных индексах ничего не найдено. Обновите их, чтобы увидеть новые чарты.
helm-charts-none-found = Чарты не найдены
helm-charts-summary =
    { $what ->
        [versions]
            { $count ->
                [one] { $count } версия в кешированных индексах
                [few] { $count } версии в кешированных индексах
                [many] { $count } версий в кешированных индексах
               *[other] { $count } версии в кешированных индексах
            }
       *[charts]
            { $count ->
                [one] { $count } чарт в кешированных индексах
                [few] { $count } чарта в кешированных индексах
                [many] { $count } чартов в кешированных индексах
               *[other] { $count } чарта в кешированных индексах
            }
    }
helm-charts-summary-cut =
    { $what ->
        [versions] Версий: { $count }; показаны первые { $shown }, уточните поиск
       *[charts] Чартов: { $count }; показаны первые { $shown }, уточните поиск
    }
helm-charts-repo-chip = репозиторий: { $name }
helm-charts-all-versions = Все версии
helm-install-oci = Установить из OCI или URL…
helm-install-oci-tooltip = Установить чарт oci://, путь или URL без репозитория
helm-charts-refresh-tooltip = Перечитать репозитории и кешированные индексы
helm-chart-deprecated = Устарел. { $description }
helm-chart-status-deprecated = устарел

## Карточка чарта

helm-back-charts = Чарты
helm-chart-not-found = Чарт не найден
helm-chart-not-found-description = Его больше нет в кешированном индексе репозитория.
helm-chart-version = Версия { $version }
helm-chart-version-app = { $version }  (приложение { $app })
helm-install-button = Установить…
helm-card-readme = README
helm-card-default-values = Values по умолчанию
helm-readme-source = Исходник
helm-readme-source-tooltip = Показать исходный Markdown
helm-card-needs-helm = Нужен helm CLI
helm-card-needs-helm-description = README и values по умолчанию берутся из helm show.
helm-card-load-error = Не удалось загрузить
helm-card-no-readme = Нет README
helm-card-no-values = Нет values по умолчанию

## Панель установки

helm-install-chart-title = Установка чарта
helm-install-chart-placeholder = oci://ghcr.io/org/charts/name, путь или URL
helm-install-release-placeholder = имя релиза
helm-install-namespace-placeholder = существующий или новый namespace
helm-version-latest = последняя
helm-label-version = версия
helm-label-release = Релиз
helm-label-namespace = namespace
helm-install-pick-namespace = Выбрать существующий namespace
helm-install-create-namespace = Создать namespace (--create-namespace)
helm-install-namespace-new = Новый namespace { $namespace }: установка создаст его (--create-namespace).
helm-install-namespaces-loading = Загрузка namespace, чтобы проверить, есть ли { $namespace }…
helm-install-namespace-checking = Проверка, есть ли namespace { $namespace }…
helm-install-namespace-unverified-forbidden = Не удалось проверить, есть ли namespace { $namespace } (нет прав на чтение): установка создаст его, если его нет (--create-namespace). Если он есть, а создавать namespace вам нельзя, снимите флажок.
helm-install-namespace-unverified = Не удалось проверить, есть ли namespace { $namespace } (проверка не удалась): установка создаст его, если его нет (--create-namespace). Если он есть, а создавать namespace вам нельзя, снимите флажок.
helm-values-user = Пользовательские values. Загрузите значения чарта по умолчанию, чтобы начать с них.
helm-values-defaults-loading = Загрузка значений по умолчанию { $chart }…
helm-values-from-defaults = Пользовательские values на основе значений чарта по умолчанию (helm show values).
helm-values-edited = Values изменены; они начинались со значений по умолчанию { $chart }.
helm-values-defaults-failed = Не удалось загрузить значения по умолчанию { $chart }: { $error }
helm-values-defaults-button = По умолчанию
helm-values-defaults-tooltip = Заменить values значениями чарта по умолчанию
helm-values-defaults-loading-title = Загрузка значений чарта по умолчанию…
helm-values-error = Values: { $error }
helm-install-enter-chart = Укажите чарт: oci://…, путь или URL.
helm-install-fix-values = Исправьте values: это некорректный YAML.
helm-no-preview = helm не вернул предпросмотр
helm-preview-interrupted = предпросмотр прерван: { $error }
helm-preview = Предпросмотр
helm-preview-failed = Предпросмотр не удался
helm-install-failed = Установка не удалась
helm-install-failed-note = Измените параметры и установите снова или сначала посмотрите предпросмотр. Если helm успел создать релиз, он есть в «Релизах» со статусом failed.
helm-install-preview-title = Предпросмотр установки (необязательно)
helm-install-preview-description = Отрисовывает чарт с этими values (helm install --dry-run=server) и показывает объекты, которые он создаст. В кластере ничего не меняется. Можно сразу установить.
helm-install-rendering = Отрисовка чарта…
helm-install-preview-failed-note = В кластере ничего не изменилось. Установка доступна и без предпросмотра.
helm-install-preview-stale = Параметры изменились после этого предпросмотра.
helm-install-preview-summary =
    { $count ->
        [one] { $name } в { $namespace } · ревизия { $revision } · чарт { $chart } · { $count } объект
        [few] { $name } в { $namespace } · ревизия { $revision } · чарт { $chart } · { $count } объекта
        [many] { $name } в { $namespace } · ревизия { $revision } · чарт { $chart } · { $count } объектов
       *[other] { $name } в { $namespace } · ревизия { $revision } · чарт { $chart } · { $count } объекта
    }
helm-install-no-objects = Чарт не создаёт объектов
helm-install-no-objects-description = Выполнятся только хуки или заметки, если они есть.
helm-install-title = Установить { $name }?
helm-install-confirm-message =
    { $create ->
        [yes]
            { $previewed ->
                [yes] Helm установит { $chart } как релиз { $release } в namespace { $namespace } (будет создан, если его нет), как в предпросмотре.
               *[no] Helm установит { $chart } как релиз { $release } в namespace { $namespace } (будет создан, если его нет) без предпросмотра: чарт не отрисовывался на кластере заранее.
            }
       *[no]
            { $previewed ->
                [yes] Helm установит { $chart } как релиз { $release } в namespace { $namespace }, как в предпросмотре.
               *[no] Helm установит { $chart } как релиз { $release } в namespace { $namespace } без предпросмотра: чарт не отрисовывался на кластере заранее.
            }
    }
helm-install-confirm = Установить
helm-install-toast = Установка { $name }
helm-missing-crds =
    { $count ->
        [one] Чарт создаёт ресурсы { $kinds }, но в кластере нет CRD для них. Обычно сначала отдельным чартом ставят оператор, который их определяет. Если чарт сам содержит CRD (папка crds/), падает только предпросмотр: dry run не создаёт CRD, поэтому можно установить без предпросмотра.
       *[other] Чарт создаёт ресурсы { $kinds }, но в кластере нет CRD для них. Обычно сначала отдельным чартом ставят оператор, который их определяет. Если чарт сам содержит CRD (папка crds/), падает только предпросмотр: dry run не создаёт CRD, поэтому можно установить без предпросмотра.
    }

## Панель обновления

helm-upgrade-chart-placeholder = repo/chart, oci://…, путь или URL
helm-version-app = приложение { $app }
helm-version-prerelease = предварительная
helm-version-current = текущая
helm-upgrade-stored-loading = Загрузка чарта, сохранённого в релизе…
helm-upgrade-stored-error = Сохранённый чарт: { $reason }
helm-upgrade-no-repository = Ни в одном репозитории нет этого чарта.
helm-upgrade-pick-version = Выберите версию чарта.
helm-upgrade-enter-chart = Укажите чарт: repo/chart, oci://…, путь или URL.
helm-upgrade-source-stored-loading = Чарт из релиза (загрузка…)
helm-upgrade-source-stored = Чарт из релиза: { $chart }
helm-upgrade-source-stored-unavailable = Чарт из релиза (недоступен)
helm-upgrade-source-repository = Чарт из репозитория { $repository }
helm-upgrade-source-any-repository = Чарт из репозитория
helm-upgrade-source-reference = Чарт из OCI-реестра, по пути или URL
helm-upgrade-stored-unusable = Чарт, сохранённый в релизе, нельзя использовать: { $reason }.
helm-upgrade-note-repository = Развёрнут: { $chart }. Версии берутся из кешированного индекса репозитория (его обновляет helm repo update); чарт скачивает helm.
helm-upgrade-note-reference = Развёрнут: { $chart }. Ни в одном из ваших репозиториев нет этого чарта; helm находит его по ссылке с вашим логином в реестре.
helm-upgrade-preview-title = Предпросмотр обновления
helm-upgrade-preview-description = Отрисовывает чарт с этими values на сервере (helm upgrade --dry-run=server) и показывает, как изменится манифест. В кластере ничего не меняется.
helm-upgrade-rendering = Отрисовка обновления…
helm-upgrade-preview-stale = Параметры изменились после этого предпросмотра; посмотрите предпросмотр снова, чтобы обновить.
helm-upgrade-preview-summary = Ревизия { $from } → { $to } · чарт { $chart }
helm-upgrade-no-change = Манифест не меняется
helm-upgrade-no-change-description = Хуки, заметки и сохранённые values всё равно могут измениться.
helm-upgrade-values-note = Пользовательские values. Они заменяют values релиза поверх значений чарта по умолчанию (--reset-values).
helm-upgrade-preview-button = Предпросмотр изменений

## Версии OCI-чартов

helm-oci-listing = Получение версий из реестра…
helm-oci-list-failed = Не удалось получить версии: введите вручную
helm-oci-no-versions = В реестре нет версий этого чарта
helm-oci-pick = Выбрать версию из реестра
helm-oci-list-error = Не удалось получить версии чарта: { $error }. Введите версию.

## Проверка полей

helm-invalid-repository-name-empty = Введите имя репозитория.
helm-invalid-repository-name-chars = Имя репозитория не может содержать «/» и пробелы.
helm-invalid-release-empty = Введите имя релиза.
helm-invalid-release-too-long = Имя релиза длиннее { $max } символов.
helm-invalid-release-chars = Имя релиза может содержать только строчные латинские буквы, цифры, «-» и «.» и должно начинаться и заканчиваться буквой или цифрой.
helm-invalid-namespace-empty = Введите namespace.
helm-invalid-namespace-chars = Namespace может содержать только строчные латинские буквы, цифры и «-», не длиннее 63 символов.
helm-invalid-oci-repository = OCI-реестры не добавляются как репозитории; устанавливайте чарт oci:// по ссылке.
helm-invalid-url-scheme = Введите URL http:// или https://.
helm-invalid-url = Введите корректный URL репозитория.
helm-invalid-insecure-credentials = Имя пользователя и пароль отправляются только по https://. Укажите URL https:// или оставьте учётные данные пустыми.
helm-invalid-yaml-line = строка { $line }: { $message }
helm-invalid-values-documents = values должны быть одним YAML-документом
helm-invalid-values-mapping = строка { $line }: values должны быть словарём
helm-invalid-no-chart-metadata = в релизе нет метаданных чарта
helm-invalid-subcharts = у чарта есть сабчарты, которые Helm не хранит в релизе; обновите его из репозитория или по пути

## Скачивание helm (подсказка на страницах Helm)

helm-tools-downloading-helm = Скачивание Helm { $version }
helm-tools-download-helm = Скачать Helm { $version }
helm-tools-download-line = Скачать { $line }
helm-tools-settings = Настройки…
helm-tools-helm-needed = Для изменений нужен helm CLI.
helm-tools-download-tooltip = Официальный релиз с get.helm.sh, проверенный подписью мейнтейнеров Helm и SHA-256, хранится в папке данных Kubach; в систему ничего не устанавливается
helm-tools-no-data-dir = Нет папки данных для скачивания.
helm-tools-check-stopped = Проверка остановлена: { $error }
helm-tools-download-stopped = Скачивание остановлено: { $error }
helm-tools-remove-failed = Не удалось удалить скачанный helm: { $error }

## Настройки → Внешние инструменты

helm-tools-auto-downloaded = Авто: { $path } (скачан)
helm-tools-auto = Авто: { $path }
helm-tools-auto-not-found = Авто: не найден в PATH
helm-tools-helm-binary = Бинарь helm
helm-tools-helm-binary-description = Используется для операций Helm, которым нужен CLI (ADR 0006). Если пусто — ищется в PATH, затем копия, скачанная Kubach.
helm-tools-helm-in-use = Используемый helm
helm-tools-helm-in-use-missing = Не найден: обновление, откат и удаление недоступны; просмотр релизов работает.
helm-tools-helm-in-use-description =
    { $source ->
        [settings] { $path } — задан выше, { $version }
        [path] { $path } — найден в PATH, { $version }
       *[downloaded] { $path } — скачан Kubach, { $version }
    }
helm-tools-version-unknown = версия неизвестна
helm-tools-helm-line = Какую линейку Helm скачивать
helm-tools-helm-line-description = Helm 4 — текущая линейка; Helm 3 получает только исправления. Kubach работает с обеими, в том числе если helm найден в PATH.
helm-tools-chart-icons = Загружать иконки чартов
helm-tools-chart-icons-description = Скачивает иконки чартов и изображения README по ссылкам из чартов и кеширует их рядом с настройками. Если выключено, Kubach не делает таких запросов (ADR 0006).
helm-tools-downloaded = Скачан Kubach
helm-tools-check-updates = Проверить обновления
helm-tools-remove = Удалить
helm-tools-remove-title = Удалить скачанный Helm?
helm-tools-remove-message = Удаляет копию Helm из папки данных Kubach. Затем действия Helm используют helm из PATH или путь, заданный выше, если он есть; скачать его снова можно здесь.
helm-tools-signed-by = Подписан: { $signer }.
helm-tools-checked-pinned-unsigned = Проверен по SHA-256, зашитому в Kubach; подпись не проверена: { $note }.
helm-tools-checked-pinned = Проверен по SHA-256, зашитому в Kubach.
helm-tools-checked-sha = Проверен по SHA-256.
helm-tools-checking = Проверка обновлений…
helm-tools-check-failed = Не удалось проверить обновления: { $error }.
helm-tools-checked-ago = Проверено { $age } назад.
helm-tools-latest-unknown = get.helm.sh не сообщил последнюю версию { $line }.
helm-tools-not-checked = Обновления ещё не проверялись.
helm-tools-unsupported = Для этой ОС и процессора нет сборки Helm: установите helm сами.
helm-tools-no-data-folder = Нет папки данных для скачивания Helm.
helm-tools-absent = Не скачан. Доступен Helm { $available }. { $checked } Официальный релиз с get.helm.sh, проверенный подписью OpenPGP мейнтейнеров Helm и SHA-256, хранится в папке данных Kubach; в систему ничего не устанавливается.
helm-tools-current = Helm { $version } в { $path }. { $verification } Актуальная версия. { $checked }
helm-tools-outdated = Helm { $version } в { $path }. { $verification } Доступен Helm { $available }. { $checked }
helm-tools-update-to = Обновить до { $version }

## Уведомления об изменениях в статус-баре (#459)

helm-notice-install-started = Установка { $name } началась
helm-notice-install-done = { $name } установлен
helm-notice-install-failed = Не удалось установить { $name }: { $error }
helm-notice-upgrade-started = Обновление { $name } началось
helm-notice-upgrade-done = { $name } обновлён
helm-notice-upgrade-failed = Не удалось обновить { $name }: { $error }
helm-notice-rollback-started = Откат { $name } начался
helm-notice-rollback-done = { $name } откачен
helm-notice-rollback-failed = Не удалось откатить { $name }: { $error }
helm-notice-uninstall-started = Удаление { $name } началось
helm-notice-uninstall-done = { $name } удалён
helm-notice-uninstall-failed = Не удалось удалить { $name }: { $error }
