# Настройки → Кластеры: страница «Для всех» (файлы kubeconfig, импортированные
# kubeconfig) и «Для каждого» (плитка, namespace, Prometheus, Helm,
# безопасность, удаление); Настройки → Заготовки.

## «Для всех»: файлы kubeconfig

settings-kubeconfig-loading = Загрузка kubeconfig…
settings-kubeconfig-not-loaded = kubeconfig не загружен: { $error }
settings-kubeconfig-files = Файлы kubeconfig
settings-kubeconfig-files-description = KUBECONFIG (или ~/.kube/config) читается всегда. Связанные файлы читаются на месте и объединяются после него.
settings-link-file = Связать файл…
settings-link-file-tooltip = Читать на месте; учётные данные остаются в вашем файле
settings-import = Импорт…
settings-import-tooltip = Скопировать в зашифрованное хранилище
settings-startup-context-missing = { $name } (нет в kubeconfig)
settings-startup-context = Контекст при запуске
settings-startup-context-description = Открывается при запуске Kubach.
settings-keep-connected = Сколько контекстов держать подключёнными
settings-keep-connected-description = Недавние контексты остаются подключёнными, поэтому возврат к ним мгновенный.
settings-find-again = Найти заново…
settings-unlink = Отвязать…
settings-no-contexts = Нет контекстов
settings-no-contexts-match = Нет подходящих контекстов
settings-file-contexts =
    { $count ->
        [one] { $count } контекст: { $contexts }
        [few] { $count } контекста: { $contexts }
       *[other] { $count } контекстов: { $contexts }
    }
settings-restrict-permissions = Ограничить до 0600

## Откуда контекст

settings-origin-linked = Связанный файл
settings-origin-imported = Импортирован
settings-origin-unknown = Неизвестно
settings-origin-your-kubeconfig = вашего kubeconfig
settings-badge-linked = связан
settings-badge-vault = хранилище

## Импортированные kubeconfig (хранилище)

settings-group-vault = Импортированные kubeconfig
settings-vault-group-description = Копии хранятся в зашифрованном виде в vault.kubach в папке настроек. Разблокируются паролем хранилища или системной связкой ключей.
settings-vault = Хранилище
settings-vault-missing = Ещё не создано: его создаст первый импорт.
settings-vault-locked = Заблокировано.
settings-vault-unlocked = Разблокировано: импортированные контексты доступны.
settings-vault-unavailable = В ОС нет папки настроек.
settings-vault-lock-now = Заблокировать
settings-vault-unlock = Разблокировать…
settings-vault-update = Обновить из файла…
settings-vault-rename = Переименовать…
settings-vault-export = Экспорт…
settings-vault-remove = Удалить
settings-vault-import-file = Импортировать файл…
settings-vault-paste = Вставить
settings-vault-paste-tooltip = Импортировать YAML kubeconfig из буфера обмена
settings-vault-keychain = Запомнить в системной связке ключей
settings-vault-keychain-busy = Запомнить в системной связке ключей…
settings-vault-keychain-checking = Проверка системной связки ключей…
settings-vault-keychain-description = Хранит ключ хранилища в Связке ключей (macOS), Диспетчере учётных данных (Windows) или Secret Service (Linux), чтобы пароль не спрашивался.
settings-vault-asks-password = Чтобы изменить это, сначала нужен пароль хранилища.
settings-vault-biometric = Разблокировать через { $method }
settings-vault-biometric-busy = Разблокировать через { $method }…
settings-vault-biometric-checking = Проверка { $method }…
settings-vault-biometric-description = Спрашивает { $method } вместо пароля. Пароль по-прежнему работает и спрашивается, если { $method } отменён или отпечатки изменились.
settings-vault-set-password-first = Сначала задайте пароль хранилища.
settings-vault-lock-screen = Блокировать вместе с экраном
settings-vault-lock-screen-description = Блокирует хранилище, как только появляется экран блокировки ОС.
settings-vault-lock-idle = Блокировать при бездействии
settings-vault-lock-idle-description = Минуты без ввода до блокировки хранилища; 0 — никогда.
settings-vault-password = Пароль хранилища
settings-vault-password-description = Всё будет заново зашифровано новым паролем.
settings-vault-no-password-description = У хранилища нет пароля, только системная связка ключей. Задайте пароль, чтобы разблокировать его где угодно.
settings-vault-encrypting = Шифрование…
settings-vault-change-password = Изменить…
settings-vault-set-password = Задать…
settings-vault-reset = Сбросить хранилище
settings-vault-reset-description = Удаляет хранилище и все импортированные kubeconfig, например после забытого пароля или повреждения файла.
settings-vault-deleting = Удаление…
settings-vault-reset-button = Сбросить…

## «Для каждого»

settings-group-cluster = Кластер
settings-no-contexts-notice = Нет контекстов kubeconfig. Добавьте kubeconfig в разделе «Кластеры: для всех»; его контексты настраиваются здесь.
settings-group-contexts = Контексты
settings-context-shown-below = Показан ниже
settings-context-open = Открыть
settings-context-hidden = Скрыт из хотбара
settings-no-api-server = В kubeconfig нет API-сервера
settings-source = Источник
settings-location-imported = Хранится в зашифрованном хранилище как «{ $entry }».
settings-location-linked = { $path }, читается на месте.
settings-location-own = { $path }, ваш kubeconfig.
settings-show-in-hotbar = Показывать в хотбаре
settings-show-in-hotbar-description = Скрытый контекст остаётся в меню «…» хотбара.

## Плитка хотбара

settings-group-hotbar-tile = Плитка хотбара
settings-hotbar-tile-description = Подпись и цвет плитки контекста в хотбаре.
settings-tile-initials-placeholder = Инициалы
settings-tile-reset = Сбросить
settings-tile-preview = Просмотр
settings-tile-default-color = Цвет по умолчанию: { $color }.
settings-tile-preset-color = Готовый цвет: { $color }.
settings-tile-custom-color-value = Свой цвет { $rgb }.
settings-tile-presets = Готовые цвета
settings-tile-presets-description = Цвета темы; они меняются вместе с тёмной и светлой темой.
settings-tile-custom = Свой цвет
settings-tile-custom-description = Любой цвет, одинаковый в обеих темах. Начинается с текущего.
settings-tile-label = Подпись
settings-tile-label-description =
    { $max ->
        [one] До { $max } символа на плитке; пусто — инициалы. Также «Переименовать…» в меню плитки.
       *[other] До { $max } символов на плитке; пусто — инициалы. Также «Переименовать…» в меню плитки.
    }

## Namespace по умолчанию, Prometheus, Helm

settings-group-default-namespace = Namespace по умолчанию
settings-default-namespace-description = Namespace, который открывается для контекста. Пусто — из kubeconfig.
settings-prometheus-description = История потребления в деталях и CPU / Memory в таблицах без metrics-server. «Авто» один раз ищет в кластере и запоминает найденное; «Найти заново» ищет снова. Адрес — http(s)://host:port или namespace/service:port (через API-сервер).
settings-prometheus-placeholder = http(s)://host:port или namespace/service:port
settings-prometheus-auto = Авто
settings-prometheus-unusual = { $address } (необычный namespace)
settings-prometheus-address = Адрес
settings-prometheus-off = Выключен
settings-prometheus-searching = Поиск в кластере…
settings-prometheus-search-failed = Поиск не удался: { $error }
settings-prometheus-not-searched = Ещё не искали
settings-prometheus-searched-none = Искали { $age } назад: Prometheus не найден
settings-prometheus-searched-found = Искали { $age } назад: { $found }
settings-prometheus-status-off = Выключен: без поиска и запросов
settings-prometheus-status-ready = подключён к { $endpoint }
settings-prometheus-status-unreachable = { $endpoint } не отвечает
settings-prometheus-status-connecting = используется { $endpoint }
settings-prometheus-status-no-address = введите адрес
settings-prometheus-status-invalid = неверный адрес: { $address }
settings-prometheus-status-unconfirmed = ждёт подтверждения { $endpoint }
settings-prometheus-unusual-warning = Найден только в namespace { $namespace }, где Prometheus обычно не ставят. Любой, кто может создать там Service, может показать выдуманное потребление. Используйте, только если доверяете ему.
settings-prometheus-use-it = Использовать
settings-prometheus-use-it-tooltip = Использовать { $endpoint } для { $name }
settings-prometheus-source-label = Prometheus для { $name }: { $label }
settings-prometheus-detect-again = Найти заново
settings-prometheus-detect-tooltip = Поискать серверы Prometheus в кластере
settings-group-helm-driver = Драйвер хранения Helm
settings-helm-driver-description = Где Helm хранит релизы (HELM_DRIVER). «Определить» берёт хранилище, где уже есть релизы, иначе Secret.
settings-helm-driver-detect = Определить

## Удаление контекста

settings-group-remove = Удаление
settings-remove-from-kubach = Удалить из Kubach
settings-remove-from-kubach-description = Удаляет импортированную копию из хранилища. Её контексты пропадут из Kubach: { $contexts }.
settings-remove-from-kubach-button = Удалить из Kubach…
settings-unlink-file = Отвязать файл
settings-unlink-file-button = Отвязать файл…
settings-unlink-description-single = Kubach перестанет читать { $path }. Файл не изменится.
settings-unlink-description =
    { $count ->
        [one] Kubach перестанет читать { $path }, поэтому пропадёт { $count } его контекст: { $contexts }. Файл не изменится.
        [few] Kubach перестанет читать { $path }, поэтому пропадут все { $count } его контекста: { $contexts }. Файл не изменится.
       *[other] Kubach перестанет читать { $path }, поэтому пропадут все { $count } его контекстов: { $contexts }. Файл не изменится.
    }
settings-hide = Скрыть
settings-hide-button = Скрыть…
settings-hidden = Скрыт
settings-hide-description = { $name } берётся из { $origin }, который Kubach не меняет. Удалите его там командой kubectl config delete-context или скройте из хотбара.
settings-hide-title = Скрыть { $name }?
settings-hide-message = Плитка уйдёт из хотбара; kubeconfig не изменится. Вернуть её можно из меню «…» хотбара или здесь.

## Безопасность контекста

settings-profile-read-only = Только чтение
settings-profile-debug = Отладка
settings-profile-deploy = Изменения
settings-deploy-dry-run = Только пробный запуск
settings-deploy-apply = Применять после подтверждения
settings-exec-never = Никогда
settings-exec-single = Отдельные команды
settings-exec-shell = Отдельные команды и shell
settings-namespaces-one = Один
settings-namespaces-several = Несколько
settings-agent-secrets-none = Ничего
settings-agent-secrets-metadata = Метаданные и проверки
settings-agent-secrets-reveal = Метаданные, проверки и раскрытие по запросу
settings-security-preset = Набор
settings-security-preset-strict = Строгий: значения по умолчанию.
settings-security-preset-development = Разработка: всё разрешено, каждое изменение подтверждается, значения Secret показываются и раскрываются агенту по запросу.
settings-security-preset-custom = Свои значения.
settings-preset-strict = Строгий
settings-preset-development = Разработка
settings-group-security = Безопасность
settings-security-description-no-server = Что может ИИ-агент, подключённый по MCP. Новые контексты строгие. В kubeconfig нет API-сервера.
settings-security-description = Что может ИИ-агент, подключённый по MCP. Новые контексты строгие. Настройки привязаны к этому имени и серверу { $server }.
settings-security-loosened = Мягче значений по умолчанию для { $name }: { $list }.
settings-security-relaxed = Ослабленная безопасность
settings-security-suggest-development = Похоже на локальный кластер. Предлагается набор «Разработка»; сам он не применяется.
settings-group-secrets = Secret
settings-hide-secret-values = Скрывать значения Secret
settings-hide-secret-values-description = Для Secret этого контекста нет «Показать», «Копировать» и «Загрузить значение», а в Helm и Kustomize нет раскрытия. Значения по-прежнему можно заменить, не загружая их.
settings-group-agent-mcp = ИИ-агент (MCP)
settings-agent-mcp-description = Агент начинает без прав. Он называет namespace, и Kubach спрашивает вас, когда агент впервые читает его, раскрывает Secret или что-то меняет. Эти настройки — максимум, который он может здесь получить.
settings-mcp-enabled = Доступ ИИ-агента
settings-mcp-enabled-description = Выключено: на этом кластере не начинается ни одна сессия агента.
settings-ceiling-raised = Повышен по запросу агента { $time } сессией { $session }: { $changes }.
settings-ceiling-raised-client = Повышен по запросу агента { $time } сессией { $session } ({ $client }): { $changes }.
settings-ceiling-raised-earlier =
    { $count ->
        [one] (ещё { $count } повышение в файле настроек)
        [few] (ещё { $count } повышения в файле настроек)
       *[other] (ещё { $count } повышений в файле настроек)
    }
settings-ceiling-raised-title = Потолок повышен из сессии
settings-protected = Защищённый кластер
settings-protected-description = Агент не может попросить из сессии поднять ограничения ниже: такие вызовы отклоняются с объяснением, а вас не спрашивают. Выключить это можно только здесь. Для production.
settings-max-profile = Максимум, который может попросить агент
settings-max-profile-description = Только чтение: просмотр ресурсов. Отладка: ещё логи и exec. Изменения: ещё изменения. Выше этого агент может попросить вас в Kubach поднять уровень — на одну сессию или навсегда, если кластер не защищён.
settings-deploy-mode = Изменения
settings-deploy-mode-description = С уровнем «Изменения». Пробный запуск: агент видит diff, ничего не меняется. Применение: каждое изменение выполняется после вашего подтверждения, включая Secret со значениями, которые вы вводите в Kubach.
settings-exec-mode = Команды в контейнерах
settings-exec-mode-description = С уровнем «Отладка» или «Изменения». Каждую команду подтверждаете вы.
settings-agent-secrets = Secret для агента
settings-agent-secrets-hidden-description = Имена, ключи, размеры и проверки внутри Kubach, но не значения. Для раскрытия нужно выключить «Скрывать значения Secret»: агент не получит значение, которое не можете видеть вы.
settings-agent-secrets-description = Раскрытие спрашивает вас о каждом Secret и предупреждает, что значения уйдут провайдеру модели. Остальные инструменты всегда скрывают значения.
settings-agent-other-kinds = Пользовательские ресурсы (CRD)
settings-agent-other-kinds-description = Выключено: только встроенные kind, такие как Pod, Deployment, Service, ConfigMap, Job и Ingress. Включено: ещё и пользовательские ресурсы, в которых могут быть учётные данные или которые заставят контроллер (Flux, Argo CD) действовать со своими правами. Secret, ServiceAccount и RBAC остаются недоступны.
settings-namespaces = Namespace на сессию
settings-namespaces-description = Один: первый разрешённый вами namespace — единственный, который получит сессия. Несколько: о каждом новом namespace спрашивается отдельно.
settings-session-ttl = Длительность сессии
settings-session-ttl-description = Сессия и все права в ней заканчиваются через это время (не больше 8 ч) или при выходе из Kubach. Сессии `kubach mcp` длятся 30 минут, если не запущены с бо́льшим --ttl.
settings-group-enforcement = Проверка на стороне кластера
settings-enforcement-description = Kubach сам проверяет каждый вызов агента. Эти настройки добавляют вторую проверку API-сервером, чтобы ошибка в Kubach не дала агенту больше, чем вы разрешили.
settings-temporary-objects = Отдельная идентичность для агента
settings-temporary-objects-description = В каждом namespace, который вы разрешили агенту читать, Kubach создаёт ServiceAccount, чья Role содержит ровно разрешённое, и удаляет его в конце сессии. Нужны права администратора в namespace.
settings-require-server-enforced = Требовать её
settings-require-server-enforced-description = Отказывать в namespace, где кластер не может обеспечить ограничения. Выключено: такой namespace помечается «ограничения только в Kubach», и каждое изменение в нём подтверждается по одному.

## Заготовки

settings-preset-name-placeholder = Имя заготовки
settings-presets-none = Заготовок пока нет
settings-presets-none-description = Откройте манифест в редакторе YAML и нажмите «Сохранить как заготовку». Заготовки предлагает «+» на странице ресурсов, по kind.
settings-presets-kind-description = { $api_version }. Заготовка по умолчанию — то, что открывает «+» на странице { $kind }.
settings-preset-default = По умолчанию
settings-preset-delete = Удалить заготовку
settings-preset-lines =
    { $lines ->
        [one] { $lines } строка YAML.
        [few] { $lines } строки YAML.
       *[other] { $lines } строк YAML.
    }
settings-preset-lines-default =
    { $lines ->
        [one] { $lines } строка YAML. Открывается «+» на странице { $kind }.
        [few] { $lines } строки YAML. Открывается «+» на странице { $kind }.
       *[other] { $lines } строк YAML. Открывается «+» на странице { $kind }.
    }
