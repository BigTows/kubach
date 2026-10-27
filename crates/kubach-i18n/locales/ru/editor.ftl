# Редакторы: YAML-редактор с проверкой и применением, ключи ConfigMap,
# редактор и значения Secret, пресеты и шаблоны новых ресурсов, диалог
# «Новый namespace». Kind, имена, контексты, пути полей и сообщения
# кластера — данные.

## Общее

editor-loading = Загрузка…
editor-reload = Обновить
editor-review = Проверить изменения
editor-apply = Применить
editor-create = Создать
editor-creating = Создание…
editor-save = Сохранить
editor-format = Форматировать
editor-format-tooltip = Расставить отступы в JSON
editor-no-keys = Нет ключей
editor-add-key = Добавить ключ
editor-remove-key = Удалить ключ
editor-key-placeholder = ключ
editor-value-placeholder = значение
editor-key-new = новый
editor-key-changed = изменён
editor-key-removed = удалён
editor-key-unchanged = без изменений
editor-key-renamed = переименован из { $key }
editor-key-twice = Ключ { $key } указан дважды
editor-content-properties = Properties
editor-content-text = Текст
editor-content-binary = Двоичные данные
editor-value-warning = { $problem }. Сохранить всё равно можно: значение — обычный текст.
editor-size-bytes =
    { $count ->
        [one] { $count } байт
        [few] { $count } байта
        [many] { $count } байт
       *[other] { $count } байта
    }
editor-binary-data = двоичные данные, { $size }
editor-file-unreadable = Не удалось прочитать { $name }: { $error }
editor-file-too-large-configmap = { $name } больше 1 MiB — это предел для всего ConfigMap
editor-file-too-large-secret = { $name } больше 1 MiB — это предел для всего Secret
editor-field-name = Имя
editor-field-type = Тип
editor-field-keys = Ключи
editor-field-user = Пользователь
editor-field-password = Пароль
editor-field-email = Эл. почта
editor-field-server = Сервер
editor-field-certificate = Сертификат
editor-field-private-key = Закрытый ключ

## YAML-редактор: кнопки ресурса

editor-edit-yaml = Изменить YAML
editor-edit-values = Изменить значения
editor-edit-values-tooltip = Изменить ключи и значения; отправляются только изменённые ключи
editor-secret-immutable-tooltip = Этот Secret неизменяемый: его данные нельзя изменить
editor-secret-not-yaml = Secret редактируется в редакторе Secret, а не как YAML
editor-kind-not-patchable = Этот kind нельзя изменить (patch)
editor-new-secret = Новый Secret
editor-new-secret-tooltip = Secret типа Opaque, docker registry или TLS из формы

## YAML-редактор: заголовки и статус

editor-title-new = Новые ресурсы
editor-tab-new = Новый ресурс
editor-tab-new-kind = Новый { $kind }
editor-placeholder-more = другие документы…
editor-subtitle-edit = Изменение · { $context }
editor-subtitle-create = Создание · { $context } · namespace по умолчанию { $namespace }
editor-checking = Проверка…
editor-errors-warnings =
    { $errors ->
        [one] { $errors } ошибка
        [few] { $errors } ошибки
        [many] { $errors } ошибок
       *[other] { $errors } ошибки
    }, { $warnings ->
        [one] { $warnings } предупреждение
        [few] { $warnings } предупреждения
        [many] { $warnings } предупреждений
       *[other] { $warnings } предупреждения
    }
editor-warnings =
    { $count ->
        [one] { $count } предупреждение
        [few] { $count } предупреждения
        [many] { $count } предупреждений
       *[other] { $count } предупреждения
    }
editor-loading-schema = Загрузка схемы…
editor-schema-unavailable = Схема недоступна: { $error }
editor-empty = Пусто
editor-no-problems = Проблем нет
editor-line = Стр. { $line }

## YAML-редактор: панель инструментов

editor-mode-keys = Ключи
editor-mode-keys-tooltip = Каждый ключ в своём редакторе, с проверкой JSON
editor-mode-yaml-tooltip = Весь объект в виде YAML
editor-reload-tooltip = Отменить правки и загрузить текущую версию
editor-save-preset = Сохранить как пресет
editor-save-preset-tooltip = Сохранить этот YAML как пресет для новых ресурсов этого kind
editor-review-tooltip = Пробный запуск на сервере (dry run), затем diff для подтверждения (⌘S)
editor-force-apply = Применить принудительно
editor-back = Назад к редактору
editor-review-force = Проверить с force…
editor-review-force-tooltip = Повторить dry run, забрав конфликтующие поля себе

## YAML-редактор: сообщения

editor-disconnected = { $context } отключён; подключите его снова, чтобы проверить или применить.
editor-load-failed = Не удалось загрузить объект: { $error }
editor-discovery-pending = Когда открылся этот редактор, API discovery ещё не завершилось; откройте его заново, чтобы применить.
editor-nothing-to-apply = Нечего применять.
editor-some-not-applied = Некоторые объекты не применены; подробности ниже.
editor-applied = Применено: { $objects }
editor-applied-created = { $label } создан
editor-applied-unchanged = { $label } без изменений
editor-applied-configured = { $label } изменён
editor-context-closed = Отключено от { $context }. Эта вкладка относится к { $context }; подключите его снова (хотбар или боковая панель), чтобы проверить или применить. Правки сохранены.
editor-context-foreign = Эта вкладка относится к { $context }. Проверка и применение идут в { $context }, а не в кластер, выбранный на боковой панели.

## YAML-редактор: проверка текста

editor-resolve-error = Документ { $document }, строка { $line }: { $message }
editor-missing-field = Нет поля `{ $field }`
editor-unknown-kind = Кластер не обслуживает { $kind } в { $version }
editor-missing-name = Нет поля `metadata.name` (generateName при apply не поддерживается)
editor-cluster-scoped = { $kind } не относится к namespace, но указан namespace «{ $namespace }»
editor-problem-conflict = { $field }: конфликт, поле принадлежит «{ $manager }»
editor-field-required = { $schema_type } (обязательное)
editor-one-of = Одно из: { $values }

## YAML-редактор: проверка изменений

editor-value-saved-anyway = { $warning }. Значение всё равно сохраняется как текст.
editor-force-warning = Принудительное применение забирает конфликтующие поля у их текущих менеджеров. Контроллеры, Helm или GitOps-инструменты, которые ими управляют, могут откатить это изменение или продолжать его перезаписывать.
editor-no-changes = Изменений нет
editor-no-changes-description = Кластер уже соответствует этому манифесту.
editor-not-applied = Не применено
editor-entry-create = создание
editor-entry-no-changes = без изменений
editor-entry-changes =
    { $count ->
        [one] { $count } изменение
        [few] { $count } изменения
        [many] { $count } изменений
       *[other] { $count } изменения
    }
editor-entry-failed = ошибка
editor-retained = Удалены, но остались после применения: { $fields }. Server-side apply удаляет только поля, которыми управляет один Kubach; этими полями управляет и другой менеджер полей (или их заполнил сервер по умолчанию).
editor-reload-stale = Обновить (правки будут потеряны)
editor-failure-conflicts =
    { $count ->
        [one] { $label }: { $count } полем управляют другие менеджеры полей
        [few] { $label }: { $count } полями управляют другие менеджеры полей
        [many] { $label }: { $count } полями управляют другие менеджеры полей
       *[other] { $label }: { $count } полями управляют другие менеджеры полей
    }
editor-failure-stale = { $label } изменился в кластере после загрузки (не совпадает resourceVersion). Обновите его, чтобы редактировать текущую версию.

## Вкладки редакторов

editor-close-title = Закрыть вкладку и отменить правки?
editor-close-detail = В { $tab } есть неприменённые правки.
editor-discard-edits = Отменить правки

## Ключи ConfigMap

editor-keys-unavailable = Нельзя редактировать по ключам: { $error }. Исправьте в YAML.
editor-configmap-immutable = Этот ConfigMap неизменяемый (immutable: true): Kubernetes не даёт менять его данные. Чтобы изменить, удалите его и создайте новый.
editor-configmap-hint = Каждый ключ — отдельный блок: значения JSON и YAML проверяются, JSON можно отформатировать. Значение, не прошедшее проверку, всё равно сохраняется как текст. Перед применением проверка показывает пробный запуск на сервере.
editor-binary-from-file = { $size }, из файла { $file }
editor-binary-not-text = { $size }; двоичные значения нельзя редактировать как текст
editor-replace-from-file = Заменить из файла

## Шаблоны и пресеты

editor-new = Создать
editor-new-tooltip = Новый ресурс из YAML
editor-new-kind-tooltip = Новый { $kind } по шаблону
editor-templates-tooltip = Шаблоны и пресеты
editor-menu-empty = Пустой
editor-menu-builtin = { $label } (встроенный)
editor-menu-skeleton = Каркас по схеме
editor-menu-default = { $name } (по умолчанию)
editor-preset-suggested = Мой { $kind }
editor-preset-placeholder = Название пресета
editor-preset-hint = Имя и namespace станут { $name } и { $namespace }.
editor-preset-name-first = Сначала назовите пресет.
editor-preset-saved = Пресет «{ $name }» для { $kind } сохранён. Он появится в меню «+» на странице ресурсов; переименовать его или сделать пресетом по умолчанию можно в Настройки → Пресеты.
editor-preset-secret = Secret не сохраняется как пресет: настройки хранятся открытым текстом.
editor-preset-no-kind = Не удалось сохранить пресет: в первом документе нужны apiVersion и kind.

## Редактор Secret

editor-secret-new-in = Новый Secret в { $namespace }
editor-secret-kind-docker = Docker registry
editor-secret-load-failed = Не удалось загрузить Secret: { $error }
editor-secret-new-key-placeholder = новый ключ
editor-secret-key-gone-reload = Ключа { $key } больше нет в Secret; обновите его.
editor-secret-immutable-error = Этот Secret неизменяемый; его данные нельзя изменить.
editor-secret-not-loaded = Secret ещё не загружен.
editor-no-changes-to-apply = Нет изменений для применения.
editor-secret-choose-cert = Выберите файл сертификата
editor-secret-choose-key = Выберите файл закрытого ключа
editor-secret-created = Secret создан.
editor-changes-applied = Изменения применены.
editor-subtitle-edit-values = Изменение значений · { $context }
editor-subtitle-create-secret = Создание · { $context }
editor-secret-reload-tooltip = Отменить правки и загрузить ключи заново
editor-back-to-editing = Назад к редактированию
editor-secret-hidden = Значения Secret скрыты в этом контексте его настройками безопасности.
editor-secret-hidden-load = { editor-secret-hidden } Текущие значения загрузить нельзя; можно заменить значение без загрузки, добавить или удалить ключи.
editor-secret-immutable-notice = Этот Secret неизменяемый (immutable: true): Kubernetes не даёт менять его данные. Чтобы изменить, удалите его и создайте новый.
editor-secret-review-patch = Отправляются только эти ключи (merge patch по data); остальные ключи не затрагиваются. Значения при проверке никогда не показываются.
editor-secret-review-create = Будет создан новый Secret типа { $schema_type } с этими ключами. Значения при проверке никогда не показываются.
editor-secret-changes-to = Изменения в { $title }
editor-secret-hidden-added = <скрыто, добавлено>
editor-secret-hidden-changed = <скрыто, изменено>
editor-secret-removed = <удалено>
editor-secret-load-value = Загрузить значение
editor-secret-load-value-tooltip = Получить текущее значение, чтобы изменить его
editor-secret-replace = Заменить
editor-secret-replace-tooltip = Ввести новое значение, не загружая текущее
editor-secret-rename-tooltip = Переименовать ключ
editor-secret-rename-disabled = Чтобы переименовать ключ, загрузите или замените значение: новый ключ записывается вместе с ним
editor-secret-undo-tooltip = Оставить текущее значение и имя
editor-secret-file-tooltip = Взять значение из файла
editor-secret-keep = Оставить
editor-secret-from-file = Из файла { $name } ({ $size }); содержимое не показывается
editor-secret-binary = { $binary }: нельзя редактировать как текст; возьмите новое значение из файла
editor-secret-cert-file = { $file } — { $subject }, до { $until }
editor-secret-cert-not-pem = { $file } — не PEM-сертификат
editor-secret-key-file = { $file } — загружен, не показывается
editor-secret-hint-opaque = Значения вводятся обычным текстом или берутся из файлов; Kubach кодирует их сам.
editor-secret-hint-docker = Создаёт Secret kubernetes.io/dockerconfigjson. Пустой сервер — Docker Hub.
editor-secret-hint-tls = Создаёт Secret kubernetes.io/tls из PEM-файлов (tls.crt, tls.key).
editor-secret-user-placeholder = пользователь
editor-secret-password-placeholder = пароль или токен
editor-secret-optional = необязательно
editor-secret-edit-hint = Ключи, которые вы не трогали, не отправляются. Загрузите значение, чтобы изменить его, или замените без загрузки. Значения вводятся обычным текстом; Kubach кодирует их сам.

## Значения Secret (панель деталей)

editor-secret-key-gone = Ключа { $key } больше нет в Secret
editor-secret-checking-access = Проверка доступа…
editor-secret-denied = { $message } Значения показать нельзя.
editor-secret-data = Данные
editor-secret-immutable = Неизменяемый
editor-secret-immutable-yes = да — данные нельзя изменить
editor-secret-hidden-keys = { editor-secret-hidden } Ключи можно менять и без загрузки.
editor-secret-security-settings = Настройки безопасности
editor-secret-show = Показать
editor-secret-hide = Скрыть
editor-secret-copy-tooltip = Скопировать; буфер обмена очистится через { $seconds } с
editor-secret-empty = (пусто)
editor-secret-ttl-note = Значения загружаются из кластера при показе и снова скрываются через { $reveal } с. Скопированное значение удаляется из буфера обмена через { $clipboard } с, при блокировке хранилища или выходе из Kubach.
editor-secret-truncated = … (обрезано; «Скопировать» берёт значение целиком)
editor-secret-summary-registry = Учётные данные реестра
editor-secret-summary-tls = TLS-сертификат
editor-secret-summary-basic-auth = Базовая аутентификация
editor-secret-summary-what-tls = Читается только tls.crt; закрытый ключ никогда не загружается.
editor-secret-summary-what-basic-auth = Читается только имя пользователя, без пароля.
editor-secret-summary-what-registry = Показывает реестры и имена пользователей, но не пароли.
editor-secret-show-summary = Показать сводку
editor-secret-tls-key-hidden = tls.key — скрыт
editor-secret-value-hidden = скрыт
editor-secret-password-set = задан (скрыт)
editor-secret-password-none = нет
editor-cert-not-yet-valid = ещё не действует
editor-cert-expired =
    { $days ->
        [one] истёк { $days } день назад
        [few] истёк { $days } дня назад
        [many] истёк { $days } дней назад
       *[other] истёк { $days } дня назад
    }
editor-cert-expires =
    { $days ->
        [one] истекает через { $days } день
        [few] истекает через { $days } дня
        [many] истекает через { $days } дней
       *[other] истекает через { $days } дня
    }
editor-cert-valid =
    { $days ->
        [one] действует, остался { $days } день
        [few] действует, осталось { $days } дня
        [many] действует, осталось { $days } дней
       *[other] действует, осталось { $days } дня
    }
editor-cert-leaf = Конечный сертификат
editor-cert-chain = Сертификат цепочки { $index }
editor-cert-subject = Субъект
editor-cert-issuer = Издатель
editor-cert-self-signed = самоподписанный
editor-cert-valid-from = Действует с
editor-cert-valid-until = Действует до
editor-cert-status = Статус

## Новый namespace

editor-namespace-new = Новый namespace
editor-namespace-new-in = Новый namespace в { $context }
editor-namespace-forbidden = Нет прав (RBAC): { $message }
editor-namespace-name-placeholder = например, team-web
editor-namespace-labels-placeholder = Необязательно: team=web, env=dev
editor-namespace-not-connected = Нет подключения к кластеру
editor-namespace-created = Namespace { $name } создан
editor-namespace-name-hint = Строчные латинские буквы, цифры и «-», не больше 63 символов.
editor-namespace-labels = Labels
editor-namespace-labels-hint = key=value через запятую.
