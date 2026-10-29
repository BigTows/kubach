# Каркас главного окна: сайдбар, хотбар кластеров, «Назад», пустые состояния,
# уведомления и отладочный оверлей. Имена контекстов, кластеров, пользователей,
# namespace и файлов — данные. Термины Kubernetes и Helm (namespace, kubeconfig,
# Gateway API, Helm, release, chart, exec, Secret) не переводятся.

## Разделы сайдбара

nav-overview = Обзор
nav-events = События
nav-topology = Топология
nav-cluster = Кластер
nav-workloads = Рабочие нагрузки
nav-network = Сеть
nav-gateway-api = Gateway API
nav-storage = Хранилище
nav-config = Конфигурация
nav-access-control = Управление доступом
nav-custom-resources = Ресурсы CRD
nav-helm = Helm
nav-kustomize = Kustomize
nav-gitops = GitOps
nav-port-forwards = Переадресация портов
nav-permissions = Права доступа
nav-security-checks = Аудит безопасности
nav-helm-releases = Releases
nav-helm-charts = Charts
nav-back-helm-releases = Helm releases
nav-back-helm-charts = Helm charts

## «← Назад» в заголовке окна

workspace-back-tooltip = Назад: { $place } ({ $shortcut })

## Уведомления и баннеры

workspace-namespace-added = Namespace { $namespace } добавлен к показанным
workspace-undo = Отменить
workspace-shell-env-banner = Окружение оболочки не загружено: { $error }. PATH и KUBECONFIG из профиля оболочки недоступны, поэтому exec-плагины аутентификации могут не найтись. Проверьте rc-файлы оболочки или запустите Kubach из терминала.

## Пустые состояния основной области

workspace-loading-kubeconfig = Загрузка kubeconfig…
workspace-kubeconfig-not-found = Kubeconfig не найден
workspace-kubeconfig-unavailable = Файлы kubeconfig недоступны
workspace-kubeconfig-unavailable-description = Не удаётся прочитать связанные файлы kubeconfig. Найдите их заново в настройках.
workspace-open-settings = Открыть настройки
workspace-no-contexts = В kubeconfig нет контекстов
workspace-no-contexts-description = Свяжите файл kubeconfig или импортируйте его в зашифрованное хранилище — или перетащите файл сюда.
workspace-add-cluster = Добавить кластер…
workspace-no-cluster = Кластер не подключён
workspace-no-cluster-description = Выберите контекст из kubeconfig, чтобы просматривать ресурсы.
workspace-disconnected = Отключено от { $context }
workspace-connect = Подключить
workspace-connecting = Подключение к { $context }…
workspace-connect-failed = Не удаётся подключиться к { $context }
workspace-retry = Повторить
workspace-gke-plugin-missing = gke-gcloud-auth-plugin не установлен. Он нужен контекстам GKE для входа; Kubach может установить его через gcloud.
workspace-gke-gcloud-missing = Не установлены ни gke-gcloud-auth-plugin, ни Google Cloud CLI. Скачайте Google Cloud CLI в Настройки → Tools, плагин установится вместе с ним.
workspace-gke-open-tools = Открыть Настройки → Tools

## Подсказки плиток хотбара

hotbar-row-cluster = Кластер
hotbar-row-server = Сервер
hotbar-row-user = Пользователь
hotbar-row-namespace = Namespace
hotbar-row-status = Статус
hotbar-row-shortcut = Сочетание
hotbar-row-source = Источник
hotbar-row-vault = Хранилище
hotbar-row-problem = Проблема
hotbar-row-file = Файл
hotbar-row-fix = Решение
hotbar-fix-linked = Правый клик → «Найти заново…» или «Убрать файл»
hotbar-vault-locked = Заблокировано. Нажмите, чтобы разблокировать.
hotbar-state-disconnected = Отключено
hotbar-state-connecting = Подключение…
hotbar-state-connected = Подключено, { $version }
hotbar-state-error = Ошибка: { $error }

## Меню хотбара

hotbar-menu-connect = Подключить
hotbar-menu-disconnect = Отключить
hotbar-menu-rename = Переименовать…
hotbar-menu-set-color = Выбрать цвет…
hotbar-menu-hide = Скрыть
hotbar-menu-security = Настройки безопасности…
hotbar-menu-lock-imported = Заблокировать импортированные кластеры
hotbar-menu-remove = Удалить из Kubach…
hotbar-menu-unlock = Разблокировать…
hotbar-menu-kubeconfig-settings = Настройки kubeconfig…
hotbar-menu-find-again = Найти заново…
hotbar-menu-remove-file = Убрать файл
hotbar-hidden = Скрытые

## Диалог переименования плитки

hotbar-rename-title = Переименовать { $context }
hotbar-rename-description =
    { $max ->
        [one] Короткая подпись на плитке хотбара, до { $max } символа. Пусто — инициалы.
        [few] Короткая подпись на плитке хотбара, до { $max } символов. Пусто — инициалы.
        [many] Короткая подпись на плитке хотбара, до { $max } символов. Пусто — инициалы.
       *[other] Короткая подпись на плитке хотбара, до { $max } символа. Пусто — инициалы.
    }

## Отладочный оверлей (только отладочные сборки)

debug-menu-overlay = Отладочный оверлей
debug-section-frames = Кадры
debug-section-process = Процесс
debug-fps = FPS
debug-idle = простой
debug-frame = Кадр
debug-avg-max = Сред. / макс. 5 с
debug-cpu = CPU
debug-memory = Память
debug-threads = Потоки
debug-clusters = Кластеры
debug-watches = Watch-потоки
debug-cached-objects = Объекты в кэше
debug-port-forwards = Переадресации портов
debug-runtime-tasks = Задачи runtime
debug-ms = { $value } мс
debug-hide = { $shortcut } — скрыть
