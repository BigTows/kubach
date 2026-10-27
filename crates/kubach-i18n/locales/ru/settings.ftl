# Экран настроек: боковая панель, поиск и страница «Общие».

settings-back = Назад в Kubach
settings-back-tooltip = Закрыть настройки (Esc)
settings-search-placeholder = Поиск настроек
settings-filter-contexts-placeholder = Фильтр контекстов
settings-no-match = Нет настроек по запросу «{ $query }».
settings-saved-to = Сохраняются в { $path }
settings-not-saved = Нет каталога конфигурации: настройки не сохраняются.
settings-file-problem = Файл настроек

## Страницы в боковой панели

settings-nav-clusters = Кластеры
settings-section-general = Общие
settings-section-clusters = Для всех
settings-section-cluster = Для каждого
settings-section-agent = ИИ-агент
settings-section-tools = Внешние программы
settings-section-presets = Заготовки
settings-title-clusters = Кластеры: для всех
settings-title-cluster = Кластеры: для каждого
settings-description-general = Оформление, язык и хранилище.
settings-description-clusters = Для всех кластеров: файлы kubeconfig, импортированные kubeconfig, контекст при запуске и фоновые подключения.
settings-description-cluster = По одному контексту: плитка на панели, namespace по умолчанию, Prometheus, Helm, безопасность и удаление из Kubach.
settings-description-agent = Подключение ИИ-агента по MCP: HTTP-сервер, сессии, подтверждения и журнал аудита.
settings-description-tools = Программы, которые Kubach запускает на этом компьютере.
settings-description-presets = Заготовки ресурсов, сохранённые из редактора YAML; их предлагает «+» на странице ресурсов.

## Страница «Общие»

settings-group-appearance = Оформление
settings-theme = Тема
settings-theme-dark = Тёмная
settings-theme-light = Светлая
settings-theme-system = Системная
settings-theme-description = «Системная» следует светлому или тёмному оформлению ОС.

settings-group-language = Язык
settings-language = Язык интерфейса
settings-language-description = Перевод частичный: пока переведены статус-бар, меню и эта страница, остальное — на английском. Имена Kubernetes, данные кластера и вывод команд не переводятся.

settings-group-terminal = Терминал
settings-node-shell-namespace = Namespace для Shell узла
settings-node-shell-namespace-description = Куда ставится привилегированный Pod для Shell узла. Pod Security там должна разрешать привилегированные Pod.
settings-node-shell-image = Образ для Shell узла
settings-node-shell-image-description = Образ Pod для Shell узла; в нём нужен nsenter (есть в busybox и alpine).
settings-terminal-clipboard = Разрешить программам копировать в буфер обмена
settings-terminal-clipboard-description = Программы во вкладках Shell и Debug могут заменить содержимое буфера обмена (OSC 52). По умолчанию выключено: Pod мог бы подложить туда команду, которую вы потом вставите.
settings-confirm-debug = Подтверждать контейнеры Debug
settings-confirm-debug-description = Спрашивать, прежде чем Debug добавит в Pod эфемерный контейнер: его нельзя убрать, пока Pod не удалён.

settings-group-storage = Хранилище
settings-cache = Кэш
settings-cache-counting = Подсчёт…
settings-cache-description = { $size }: иконки чартов и картинки из README. Всё это скачается заново, когда понадобится; настройки и сохранённое состояние останутся.
settings-cache-description-in = { $size } в { $dir }: иконки чартов и картинки из README. Всё это скачается заново, когда понадобится; настройки и сохранённое состояние останутся.
settings-cache-clear-failed = Не удалось очистить полностью: { $error }
settings-clear-cache = Очистить кэш

## Длительности: `30 мин`, `1 ч`, `1 ч 30 мин`, `40 с`

settings-duration-minutes = { $minutes } мин
settings-duration-hours = { $hours } ч
settings-duration-hours-minutes = { $hours } ч { $minutes } мин
settings-duration-seconds = { $seconds } с
settings-unit-minutes = мин
