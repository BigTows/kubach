vault-add-cloud = Импорт из облака…
vault-add-cloud-text = Google GKE, Yandex Cloud или AWS EKS: выберите кластеры, которые видит CLI провайдера. Токены облака Kubach не хранит, контексты входят через CLI. Хранится в зашифрованном хранилище.
cloud-title = Импорт из облака
cloud-refresh = Обновить
cloud-cli-cannot-download = Kubach не может скачать { $tool } для этой системы. Установите его по инструкции { $url } и нажмите «Обновить».
cloud-cli-missing = { $tool } не найден в PATH.
cloud-cli-download = Скачать { $tool }
cloud-cli-download-tooltip = Официальный релиз с сайта производителя, проверенный, если производитель публикует контрольную сумму или подпись, в папку данных Kubach. В систему ничего не устанавливается.
cloud-cli-in-path = Используется { $path } (из PATH)
cloud-cli-downloaded = Используется { $path } (скачан Kubach)
cloud-gke-plugin-missing = gke-gcloud-auth-plugin не установлен, без него контексты GKE не смогут войти. Установка: gcloud components install gke-gcloud-auth-plugin
cloud-gke-plugin-install = Установить плагин
cloud-gke-plugin-installing = Устанавливаем плагин…
cloud-gke-plugin-failed = gke-gcloud-auth-plugin не установлен: { $error }
cloud-gke-plugin-required = Сначала установите gke-gcloud-auth-plugin (вкладка Google GKE): без него контексты GKE не смогут войти.
cloud-gke-python-hint = На macOS gcloud нужен Python 3.10 или новее: установите его (python.org или brew install python) или укажите его в CLOUDSDK_PYTHON и повторите.
cloud-not-logged-in = Вход в { $tool } не выполнен. Войдите в терминале и нажмите «Повторить»:
cloud-error = Ошибка { $tool }: { $error }
cloud-try-again = Повторить
cloud-loading = Спрашиваем { $tool }…
cloud-scopes-gke = Проекты
cloud-scopes-yandex = Каталоги
cloud-scopes-eks = Регионы
cloud-profile = Профиль
cloud-account = Вход выполнен как { $account }
cloud-no-scopes = Для этой учётной записи ничего не найдено.
cloud-find-clusters = Найти кластеры
cloud-finding = Ищем кластеры…
cloud-endpoint-external = Внешний
cloud-endpoint-internal = Внутренний
cloud-endpoint-dns = DNS
cloud-clusters =
    { $count ->
        [one] { $count } кластер
        [few] { $count } кластера
       *[other] { $count } кластеров
    }
cloud-no-clusters = В выбранных местах кластеров нет.
cloud-scope-failed = { $scope }: { $error }
cloud-note = Контексты запускают { $tool } для входа при каждом подключении Kubach; токены облака Kubach не хранит.
cloud-selected =
    { $count ->
        [one] Выбран { $count } кластер
        [few] Выбрано { $count } кластера
       *[other] Выбрано { $count } кластеров
    }
cloud-next = Далее
cloud-none-selected = Отметьте хотя бы один кластер.
cloud-tools-group = CLI облаков
cloud-tools-downloading = Скачиваем { $tool }…
cloud-tools-in-path = Используется: { $path } из PATH, { $version }.
cloud-tools-in-use-downloaded = Используется: { $path }, скачан Kubach, { $version }.
cloud-tools-missing = Не найден. Нужен для импорта из облака.
cloud-tools-downloaded = Скачанная копия: { $version }. { $verification }
cloud-tools-unsupported = Kubach не может скачать { $tool } для этой системы: установите его по инструкции { $url }.
cloud-tools-download = Скачать
cloud-tools-update = Обновить
cloud-tools-remove-title = Удалить скачанный { $tool }?
cloud-tools-remove-message = Удаляет копию из папки данных Kubach. Импортированные с ней контексты не смогут войти, пока { $tool } не установлен или не скачан снова.
cloud-tools-checked-published = Проверен по SHA-256, опубликованному на { $source }.
cloud-tools-package-signed = Пакет подписан: { $signer }.
cloud-tools-unchecked = Не проверен: производитель не публикует контрольных сумм и подписей; скачан по HTTPS с сервера производителя.
