# Хранилище импортированных kubeconfig: добавление кластера, предпросмотр
# импорта, разблокировка, смена пароля, сброс, удаление, переименование и
# экспорт, форма «Добавить вручную…» и ошибки хранилища из kubach-core.
# Имена записей, контекстов, файлов и серверов и причины от ОС — данные.
# $method — «Touch ID», «Windows Hello» или «biometrics». kubeconfig,
# Kubernetes, exec, OIDC, PEM, CA, PATH, YAML, EKS/GKE/AKS и командные
# строки не переводятся.

## Добавление кластера

vault-choose = Выбрать
vault-link-text = Читается на месте: изменения от aws, gcloud и других утилит подхватываются. Учётные данные остаются в вашем файле.
vault-import-text = Копируется в зашифрованное хранилище Kubach, которое открывается паролем или системной связкой ключей. Для файлов, которые иначе лежали бы где попало.
vault-add-title = Добавить кластер
vault-add-link = Подключить файл kubeconfig…
vault-add-import = Импортировать файл kubeconfig…
vault-add-paste = Вставить kubeconfig из буфера обмена
vault-add-paste-text = Как импорт, но из скопированного YAML (например, из облачной консоли).
vault-add-manual = Добавить вручную…
vault-add-manual-text = Укажите API-сервер и учётные данные (токен, клиентский сертификат или exec-плагин) или напишите YAML kubeconfig. Хранится в зашифрованном хранилище.
vault-dropped-title =
    { $count ->
        [one] Добавить файл kubeconfig
       *[other] Добавить файлы kubeconfig
    }
vault-dropped-link =
    { $count ->
        [one] Подключить файл
       *[other] Подключить файлы
    }
vault-dropped-import =
    { $count ->
        [one] Импортировать файл
       *[other] Импортировать файлы
    }
vault-dropped-import-several = Каждый файл копируется в зашифрованное хранилище Kubach отдельной записью, с предпросмотром для каждого по очереди.
vault-clipboard-empty = В буфере обмена нет текста. Сначала скопируйте YAML kubeconfig.

## Предпросмотр импорта

vault-import-title = Импорт kubeconfig
vault-update-title = Обновить импортированный kubeconfig
vault-pasted-name = Вставленный kubeconfig
vault-name = Имя
vault-password = Пароль хранилища
vault-password-repeat = Повторите пароль
vault-context-name = Имя контекста
vault-confirm-read-files = Сначала подтвердите чтение локальных файлов
vault-no-contexts = В kubeconfig нет контекстов
vault-fix-names = Сначала исправьте имена контекстов
vault-enter-name = Введите имя
vault-choose-password = Задайте пароль хранилища
vault-passwords-differ = Пароли не совпадают
vault-no-settings-folder = В ОС нет папки настроек для хранилища
vault-imported =
    { $count ->
        [one] { $count } контекст в зашифрованном хранилище
        [few] { $count } контекста в зашифрованном хранилище
        [many] { $count } контекстов в зашифрованном хранилище
       *[other] { $count } контекста в зашифрованном хранилище
    }
vault-renamed-taken = Переименован: «{ $original }» уже занято
vault-renamed = Переименован из «{ $original }»
vault-adds = Добавит { $contexts ->
        [one] { $contexts } контекст
        [few] { $contexts } контекста
        [many] { $contexts } контекстов
       *[other] { $contexts } контекста
    }, { $clusters ->
        [one] { $clusters } кластер
        [few] { $clusters } кластера
        [many] { $clusters } кластеров
       *[other] { $clusters } кластера
    } и { $users ->
        [one] { $users } пользователя
        [few] { $users } пользователя
        [many] { $users } пользователей
       *[other] { $users } пользователя
    }.
vault-new-note = Импортированные kubeconfig хранятся зашифрованными. Задайте пароль хранилища; Kubach спросит его один раз за сеанс.
vault-remember = Запомнить в системной связке ключей (без пароля)
vault-locked-biometric = Хранилище заблокировано. Введите его пароль или оставьте поле пустым, чтобы использовать { $method }.
vault-locked-keychain = Хранилище заблокировано. Введите его пароль или оставьте поле пустым, чтобы использовать системную связку ключей.
vault-locked-password = Хранилище заблокировано. Введите его пароль, чтобы добавить этот конфиг.
vault-locked-keychain-only = Хранилище заблокировано; оно открывается системной связкой ключей.
vault-broken = { $error } Сбросьте хранилище в Настройки → Clusters, чтобы снова импортировать.
vault-source-manual = Введено вручную
vault-source-clipboard = Из буфера обмена
vault-source-queue = Файл { $current } из { $total } · { $source }
vault-read-files = Прочитать эти файлы в хранилище
vault-back = Назад
vault-cancel = Отмена
vault-skip = Пропустить
vault-skip-tooltip = Перейти к следующему файлу, не импортируя этот
vault-encrypting = Шифрование…
vault-update = Обновить
vault-import = Импортировать
vault-import-unreadable-file = Не удалось прочитать { $path } ({ $error }); импортированный конфиг продолжит ссылаться на него.
vault-name-empty = Введите имя контекста
vault-name-duplicate = В этом импорте уже есть контекст с таким именем
vault-name-taken = Контекст с таким именем уже существует

## Разблокировка

vault-unlock-title = Разблокировать импортированные кластеры
vault-unlock-text = Импортированные kubeconfig зашифрованы. После разблокировки они хранятся в памяти, пока вы не заблокируете хранилище, не будете бездействовать или не выйдете.
vault-unlock-waiting = Подтвердите через { $method }…
vault-use-password = Ввести пароль
vault-use-biometric = Использовать { $method }
vault-use-keychain = Использовать связку ключей
vault-unlocking = Разблокировка…
vault-unlock = Разблокировать
vault-enter-password = Введите пароль
vault-busy = Хранилище занято; попробуйте через минуту
vault-wrong-password-wait = Неверный пароль. Повторите через { $seconds } с.
vault-too-many-attempts = Слишком много попыток. Повторите через { $seconds } с.
vault-missing = Хранилища ещё нет
vault-closing = Kubach закрывается
vault-locked = Хранилище заблокировано

## Пароль, сброс, удаление, переименование, экспорт

vault-password-title = Сменить пароль хранилища
vault-new-password = Новый пароль
vault-new-password-repeat = Повторите новый пароль
vault-enter-new-password = Введите пароль
vault-password-changed = Пароль хранилища изменён
vault-password-text = Всё содержимое хранилища будет заново зашифровано новым ключом.
vault-change = Сменить
vault-reset-title = Сбросить хранилище?
vault-reset-description = Зашифрованное хранилище и импортированные в него kubeconfig ({ $count }) будут удалены, а их кластеры пропадут из Kubach; вместе с ними удаляются копии повреждённых файлов хранилища (vault.kubach.corrupt-…) и его ключи в системной связке ключей. Это нельзя отменить.
vault-reset-ok = Удалить хранилище
vault-reset-keys-left = Хранилище удалено, но его ключ мог остаться в системной связке ключей: файл хранилища не читался или связка ключей отказалась его удалить. Найдите в связке ключей (Связка ключей, Диспетчер учётных данных, Пароли и ключи) «{ $keychain }» и удалите записи «{ $keychain }» и «{ $biometric }-…».
vault-remove-title = Удалить { $name }?
vault-remove-description = Его контексты ({ $contexts }) пропадут из Kubach. Других копий в хранилище нет.
vault-remove = Удалить
vault-rename-title = Переименовать импортированный kubeconfig
vault-export-title = Экспортировать как обычный kubeconfig?
vault-export-description = Экспортированный файл НЕ зашифрован: любой, кто может его прочитать, получит учётные данные. Kubach делает его доступным только вам (0600); не кладите его в резервные копии и общие папки.
vault-export-ok = Экспортировать…
vault-export-locked = Хранилище заблокировалось; разблокируйте его и экспортируйте снова.
vault-exported = Экспортировано в { $path }
vault-export-failed = Не удалось записать { $path }: { $error }

## Ошибки хранилища (kubach-core)

vault-error-wrong-password = Неверный пароль
vault-error-corrupted = Файл хранилища повреждён ({ $reason }).
vault-error-corrupted-backup = Файл хранилища повреждён ({ $reason }). Копия сохранена в { $path }.
vault-error-newer-format = Хранилище записано более новой версией Kubach (формат { $version }); обновите Kubach, чтобы открыть его
vault-error-no-password = У этого хранилища нет пароля; разблокируйте его системной связкой ключей
vault-error-no-keychain = Это хранилище не сохранено в системной связке ключей
vault-error-keychain-missing = Ключа хранилища нет в системной связке ключей; разблокируйте паролем
vault-error-keychain-mismatch = Ключ из системной связки ключей не открывает это хранилище; разблокируйте паролем
vault-error-no-key = Хранилищу нужен пароль, системная связка ключей или и то и другое
vault-error-no-biometric = Разблокировка через { $method } для этого хранилища не включена
vault-error-biometric-needs-password = Для разблокировки через { $method } нужен запасной пароль хранилища; сначала задайте его
vault-error-biometric-mismatch = Ключ { $method } не открывает это хранилище; разблокируйте паролем
vault-error-biometric-cancelled = { $method }: отменено
vault-error-biometric-invalidated = { $method } изменился с момента настройки хранилища (добавлены или удалены отпечатки), и его ключ больше не работает. Введите пароль, затем снова включите разблокировку через { $method } в Настройках.
vault-error-biometric-failed = Ошибка { $method }: { $reason }
vault-error-text = { $text }
vault-error-empty-password = Пароль пуст
vault-error-exists = Хранилище уже существует: { $path }
vault-error-unknown-entry = Нет импортированного конфига с id { $id }
vault-error-serialize = Не удалось сохранить конфиг: { $error }
vault-error-context-names = Имена контекстов должны быть непустыми, уникальными и не занятыми другими контекстами
vault-error-unread-files = Сначала подтвердите чтение локальных файлов, на которые ссылается конфиг
vault-error-io = Нет доступа к файлу хранилища: { $error }

## «Добавить вручную…»

manual-title = Добавить кластер вручную
manual-auth-token = Токен
manual-auth-cert = Клиентский сертификат
manual-auth-basic = Пароль
manual-auth-oidc = OIDC
manual-auth-exec = Exec-плагин
manual-context-placeholder = например, prod-eu
manual-token-placeholder = Bearer-токен
manual-password-placeholder = Пароль
manual-client-secret-placeholder = Только для конфиденциальных клиентов
manual-region-placeholder = eu-west-1 (по умолчанию — регион профиля)
manual-yaml-not-shown = { $why }. Форма сохраняет свои значения; на вкладке YAML остаётся ваш текст. Проверяется и сохраняется открытая вкладка.
manual-write-yaml = Сначала напишите YAML kubeconfig
manual-connecting = Подключение к { $server }…
manual-connected = Подключено: Kubernetes { $version }
manual-test-stopped = Проверка неожиданно прервалась
manual-load-file = Загрузить из файла…
manual-ca-custom = CA-сертификат
manual-ca-system = Системное хранилище сертификатов
manual-ca-insecure = Не проверять
manual-ca-pem = CA-сертификат (PEM)
manual-ca-system-note = Для серверов с сертификатом от публичного центра сертификации (у большинства управляемых кластеров за балансировщиком такого нет).
manual-insecure-warning = Сертификат сервера не проверяется: любой между вами и кластером может выдать себя за него и прочитать учётные данные. Только для локальных тестовых кластеров.
manual-token = Токен
manual-cert-pem = Клиентский сертификат (PEM)
manual-key-pem = Клиентский ключ (PEM)
manual-username = Имя пользователя
manual-password = Пароль
manual-basic-note = HTTP basic auth. В Kubernetes 1.19 его убрали из API-сервера; работает только со старыми кластерами или за аутентифицирующим прокси.
manual-issuer = URL издателя (issuer)
manual-client-id = Client ID
manual-client-secret = Client secret (необязательно)
manual-scopes = Дополнительные scopes (необязательно, через пробел)
manual-oidc-install = Установите kubelogin: kubectl krew install oidc-login (или brew install int128/kubelogin/kubelogin).
manual-oidc-note = Запускает kubelogin (kubectl oidc-login get-token) как exec-плагин: он открывает браузер для входа, затем кеширует и обновляет токены.
manual-context = Имя контекста
manual-server = URL API-сервера
manual-namespace = Namespace по умолчанию (необязательно)
manual-server-certificate = Сертификат сервера
manual-sign-in = Способ входа
manual-missing =
    { $count ->
        [one] { $names } не найден в PATH. { $hint }
       *[other] { $names } не найдены в PATH. { $hint }
    }
manual-missing-shell =
    { $count ->
        [one] { $names } не найден в PATH. { $hint } Окружение login-оболочки не загрузилось ({ $error }), поэтому PATH может быть неполным.
       *[other] { $names } не найдены в PATH. { $hint } Окружение login-оболочки не загрузилось ({ $error }), поэтому PATH может быть неполным.
    }
manual-template-custom = Свой
manual-command = Команда
manual-args = Аргументы
manual-env = Окружение (NAME=value через пробел)
manual-api-version = Версия API
manual-eks-cluster = Имя кластера EKS
manual-region = Регион (необязательно)
manual-profile = Профиль AWS (необязательно)
manual-gke-note = Использует ваш вход в gcloud (gcloud auth login). URL API-сервера и CA-сертификат — в выводе gcloud container clusters describe <cluster>.
manual-server-id = Server ID
manual-aks-note = Вход через Azure CLI (az login). Server ID одинаков для всех кластеров с Entra ID под управлением AKS.
manual-runs = Запускает: { $line }
manual-install-custom = Установите её или укажите полный путь.
manual-install-eks = Установите AWS CLI v2 и войдите (aws configure / aws sso login).
manual-install-gke = Установите: gcloud components install gke-gcloud-auth-plugin, затем gcloud auth login.
manual-install-aks = Установите kubelogin (az aks install-cli) и войдите через az login.
manual-install-yc = Установите Yandex Cloud CLI и войдите через yc init.
manual-yc-note = Вход через yc CLI (yc init); пустой профиль — активный. URL API-сервера и CA-сертификат — в выводе yc managed-kubernetes cluster get <cluster>.
manual-exec-note = Команда запускается на этом компьютере при подключении Kubach, с PATH вашей login-оболочки.
manual-template = Шаблон
manual-yaml-note = Полный kubeconfig; сертификаты и ключи встроены (*-data). Можно несколько контекстов; проверка использует current-context. Exec-плагины из него запускаются при проверке.
manual-tab-form = Форма
manual-testing = Проверка…
manual-test = Проверить подключение
manual-next = Просмотреть и сохранить…
manual-file-unreadable = Не удалось прочитать { $name }: { $error }
manual-file-too-large = { $name } слишком большой для сертификата или ключа
manual-file-not-pem = { $name } — не текстовый PEM-файл (DER-сертификаты не поддерживаются)

## Ошибки формы (kubach-core)

manual-problem-empty-context = Введите имя контекста
manual-problem-empty-server = Введите URL API-сервера
manual-problem-invalid-server = Неверный URL сервера: { $error }
manual-problem-empty-ca = Вставьте CA-сертификат (PEM) или выберите его файл
manual-problem-invalid-ca = CA — не PEM-сертификат (-----BEGIN CERTIFICATE-----)
manual-problem-empty-token = Введите токен
manual-problem-invalid-cert = Клиентский сертификат — не PEM (-----BEGIN CERTIFICATE-----)
manual-problem-invalid-key = Клиентский ключ — не закрытый ключ PEM (-----BEGIN … PRIVATE KEY-----)
manual-problem-empty-username = Введите имя пользователя
manual-problem-empty-password = Введите пароль
manual-problem-empty-issuer = Введите URL издателя
manual-problem-invalid-issuer = Неверный URL издателя: { $error }
manual-problem-empty-client-id = Введите Client ID
manual-problem-empty-cluster-name = Введите имя кластера
manual-problem-empty-server-id = Введите Server ID
manual-problem-empty-command = Введите команду плагина
manual-problem-trailing-backslash = Обратная косая черта в конце ничего не экранирует
manual-problem-unclosed-quote = Не закрыта кавычка { $quote }
manual-problem-invalid-env = Переменные окружения задаются как NAME=value: «{ $entry }»
manual-yaml-invalid = Это не kubeconfig: { $error }
manual-yaml-contexts = Форма показывает один контекст, а в этом конфиге их { $count }
manual-yaml-missing-cluster = Кластера контекста нет в конфиге
manual-yaml-unsupported = В конфиге есть { $feature }, а форма такое не показывает
manual-feature-ca-file = путь к файлу CA
manual-feature-proxy = прокси
manual-feature-tls-server-name = tls-server-name
manual-feature-basic-with-others = basic auth вместе с другими учётными данными
manual-feature-auth-provider = auth provider
manual-feature-credential-files = пути к файлам учётных данных
manual-feature-impersonation = impersonation
