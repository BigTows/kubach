# Вопросы MCP-сессии ИИ-агента в окне Kubach: одобрение изменения, права,
# релиза Helm, контекста или сессии, форма Secret и уведомления. Сводка,
# подробности, права и лимиты запроса приходят из kubach-mcp по-английски
# для агента и аудита — здесь это данные, как имена, namespace, ключи,
# инструменты и id сессий. Secret, Helm, release, chart, MCP, Pod, PATH не
# переводятся.

## Уведомления

agent-security-settings = Настройки безопасности
agent-notice-reveal-off = Показ Secrets выключен
agent-notice-refused = Агенту отказано
agent-notice-text = { $text }
agent-notice-mcp-off = { $summary }: доступ ИИ-агентов к этому контексту выключен.
agent-notice-session-only = Разрешено только на эту сессию
agent-notice-session-only-text = Файл настроек нельзя записать, поэтому доступ действует только до конца сессии.

## Заголовки диалогов

agent-title-read = Разрешить агенту чтение?
agent-title-reveal = Показать агенту Secret?
agent-title-modify = Разрешить агенту изменения?
agent-title-exec = Выполнить команду агента?
agent-title-port-forward = Открыть port-forward агента?
agent-title-access = Дать агенту больше доступа?
agent-title-helm-install = Установить релиз Helm агента?
agent-title-helm-upgrade = Обновить релиз Helm агента?
agent-title-helm-rollback = Откатить релиз Helm?
agent-title-helm-uninstall = Удалить релиз Helm?
agent-title-chart-source = Разрешить источник чартов?
agent-title-use-context = Разрешить агенту работать в этом контексте?
agent-title-call = Одобрить изменение агента?
agent-title-secret-create = Введите значения нового Secret
agent-title-secret-update = Изменение Secret
agent-title-secret-copy = Скопировать Secret для агента?
agent-title-secret-delete = Удалить Secret для агента?

## Подтверждение

agent-arg-empty = (пусто)
agent-command-header =
    { $count ->
        [one] { $count } аргумент, байт: { $bytes } · ␣ — пробел, \n — перевод строки, \u{"{"}…{"}"} — невидимый символ
        [few] { $count } аргумента, байт: { $bytes } · ␣ — пробел, \n — перевод строки, \u{"{"}…{"}"} — невидимый символ
        [many] { $count } аргументов, байт: { $bytes } · ␣ — пробел, \n — перевод строки, \u{"{"}…{"}"} — невидимый символ
       *[other] { $count } аргумента, байт: { $bytes } · ␣ — пробел, \n — перевод строки, \u{"{"}…{"}"} — невидимый символ
    }
agent-no-longer-pending = Этот запрос больше не ждёт ответа.
agent-dangerous-title = Выходит за границу namespace или читает Secrets
agent-dangerous-line = { $message } — { $path }
agent-dangerous-line-document = { $message } — документ { $document }: { $path }
agent-acknowledge = Я понимаю, что это позволяет нагрузке выйти за границу namespace или прочитать эти Secrets, и всё равно одобряю
agent-secret-leaves = Значение покинет ваш компьютер
agent-secret-no-take-back = Kubach не сможет забрать его обратно, когда агент его получит.
agent-deny = Отказать
agent-reject = Отклонить
agent-approve = Одобрить
agent-allow-always = Разрешать по умолчанию
agent-allow-session = Разрешить на эту сессию
agent-helm-install = Установить
agent-helm-upgrade = Обновить
agent-helm-rollback = Откатить
agent-helm-uninstall = Удалить
agent-dont-reveal = Не показывать
agent-reveal = Показать агенту
agent-keep-secret = Оставить
agent-delete-secret = Удалить Secret
agent-copy-secret = Скопировать Secret
agent-field-agent = Агент
agent-client-unverified = имя сообщает клиент, не проверено
agent-field-context = Контекст
agent-field-namespace = Namespace
agent-field-right = Право
agent-field-release = Релиз
agent-field-chart = Чарт
agent-field-version = Версия
agent-field-revision = Ревизия
agent-field-runs-as = От имени
agent-field-asks-for = Просит
agent-field-access-now = Есть сейчас
agent-access-details = «Разрешить» добавляет это до конца сессии. С «Разрешать по умолчанию» минимальный доступ { $context } повышается в Настройки → Security насовсем. В обоих случаях каждое изменение, удаление, действие Helm и команда по-прежнему подтверждаются по одной, и ничего не выходит за ваши права в Kubernetes.
agent-access-secrets = Secret (метаданные и проверки)
agent-access-crds = пользовательские ресурсы
agent-field-tool = Инструмент
agent-field-session = Сессия
agent-helm-values = Values (чувствительные ключи скрыты)
agent-helm-objects = Объекты
agent-always = Разрешать по умолчанию
agent-always-note = Повышает минимальный доступ в Настройки → Security для { $context } насовсем, с отметкой, когда и из какой сессии. Вернуть можно там же кнопкой «Сбросить до чтения».
agent-limits-off = Серверные ограничения выключены
agent-limits-off-note = Вызов выполняется с вашими учётными данными; проверяет его только Kubach.
agent-client-only = Граница только в Kubach: вызов выполняется с вашими учётными данными.
agent-auto-reject = Автоматический отказ через { $left }
agent-auto-reject-queued = Автоматический отказ через { $left } · ещё ждут: { $queued }

## Форма Secret

agent-writes-to = Запись в
agent-secret = Secret
agent-copied-from = Копия из
agent-secret-type = Тип
agent-secret-keys = Ключи
agent-secret-unverified = Имя и ключи пришли от агента и не проверены.
agent-explanation = Пояснение агента, не проверено
agent-still-referenced = Ещё используется: новые поды этих объектов не запустятся
agent-used-by = Используется (перезапустите, чтобы подхватить новые значения)
agent-placeholder-user = пользователь
agent-placeholder-password = пароль или токен
agent-placeholder-optional = необязательно
agent-placeholder-key = ключ
agent-placeholder-value = значение
agent-enter-value = Введите значение для { $key }, сгенерируйте его или уберите ключ
agent-enter-value-new = Введите значение для нового ключа, сгенерируйте его или уберите ключ
agent-key-exists = Ключ { $key } уже есть; измените его в его строке
agent-key-twice = Ключ { $key } задан дважды
agent-choose-tls = Выберите сертификат и закрытый ключ
agent-new-key = новый ключ
agent-value-unchanged = без изменений, { $size }
agent-value-type = введите значение
agent-value-typed =
    { $count ->
        [one] введено, { $count } символ
        [few] введено, { $count } символа
        [many] введено, { $count } символов
       *[other] введено, { $count } символа
    }
agent-value-generated =
    { $count ->
        [one] сгенерировано, { $count } символ, скрыто
        [few] сгенерировано, { $count } символа, скрыто
        [many] сгенерировано, { $count } символов, скрыто
       *[other] сгенерировано, { $count } символа, скрыто
    }
agent-value-file = из { $name } ({ $size }), не показывается
agent-value-removed = будет удалён
agent-badge-asked = просит агент
agent-badge-remove = агент предлагает удалить
agent-keep-key = Оставить этот ключ
agent-keep = Оставить
agent-type-new = Ввести новое значение
agent-type = Ввести
agent-generate-tooltip = Сгенерировать случайное значение; его никто не увидит
agent-generate = Сгенерировать
agent-copy-generated = Скопировать сгенерированное значение; буфер обмена очистится через 30 с
agent-from-file = Взять значение из файла
agent-keep-current = Оставить текущее значение
agent-remove-key = Удалить этот ключ
agent-leave-out = Не добавлять этот ключ
agent-add-key = Добавить ключ
agent-tls-cert = Сертификат (.crt)
agent-tls-key = Закрытый ключ (.key)
agent-registry = Registry
agent-user = Пользователь
agent-password = Пароль
agent-email = Email
agent-hint-tls = Выберите PEM-сертификат и его закрытый ключ; Kubach проверит, что они подходят друг к другу.
agent-hint-docker = Kubach соберёт .dockerconfigjson из этих полей. Пустой registry означает Docker Hub.
agent-hint-update = Ключи без изменений не отправляются. Введите, сгенерируйте или выберите файл для заменяемых ключей.
agent-hint-keys = Введите каждое значение, сгенерируйте его или возьмите из файла.
agent-create-secret = Создать Secret
agent-apply = Применить изменения
agent-client = Агент: { $client }
agent-client-unverified-note = (имя сообщает клиент, не проверено)
agent-secret-info = Эти значения записывает Kubach; агент получает только ключи и размеры. Вводите только значения, предназначенные для этого контекста и namespace.
agent-secret-limits = { $warning }. Запись выполняется с вашими учётными данными.
agent-auto-cancel = Автоматическая отмена через { $left }
agent-cancel = Отмена
agent-file-unreadable = Не удалось прочитать { $name }: { $error }
agent-file-too-large = { $name } больше 1 МиБ — предела всего Secret
agent-cert-valid = { $subject }, действует до { $time }
agent-cert-none = нет сертификата
agent-key-none = В этом файле нет закрытого ключа PEM
agent-key-loaded = Закрытый ключ загружен, не показывается
agent-key-matches = Подходит к сертификату
agent-key-differs = Не подходит к сертификату
agent-key-unknown = Kubach не может определить, подходит ли он к сертификату

## Начало сессии

agent-use-context-summary = { $client } хочет работать в контексте { $context }
# $access: минимальный доступ, например «Чтение».
agent-use-context-details = «Да» начинает сессию агента в { $context } до выхода из Kubach, но не дольше 8 ч. Без вопросов у неё есть: { $access }. Всё сверх этого спрашивается здесь.
agent-consent-summary = Начать сессию ИИ-агента в `{ $context }` с учётными данными из хранилища Kubach?
agent-consent-session = новая, для pid { $pid }
agent-consent-tool = начать сессию
agent-consent-namespaces = Namespace: { $namespaces }
agent-consent-namespaces-any = любые, которые назовёт агент
agent-consent-access = Без вопросов: { $access }; больше — по запросу здесь
agent-consent-time = Время: { $time } или до блокировки хранилища
agent-consent-reading = Чтение: { $reading }
agent-consent-reading-granted = разрешено сразу (--grant-read)
agent-consent-reading-asked = в пределах минимального доступа
agent-consent-process = Процесс: { $process }
agent-consent-unknown-process = pid { $pid } (ОС не показывает этот процесс)
agent-consent-warning = Pid сообщает сам процесс. Если вы только что не запускали агента для этого контекста, отклоните.

