# 0019 — Импорт кластеров из облака через CLI провайдера

Статус: принято

## Контекст
Добавить управляемый кластер (GKE, Yandex Cloud Managed Kubernetes, EKS) руками долго:
нужны endpoint, CA и exec-плагин облака. CLI провайдеров умеют записывать контексты сами
(`gcloud container clusters get-credentials`, `yc managed-kubernetes cluster get-credentials`,
`aws eks update-kubeconfig`), но пишут в `~/.kube/config`, а у пользователя CLI может и не
быть (#38).

## Решение
- «+» → Add cluster → **Import from cloud…** (`kubach-app::vault::cloud`): провайдер
  (Google GKE, Yandex Cloud, AWS EKS) → CLI → список кластеров → отмеченные кластеры
  уходят через обычный предпросмотр импорта в зашифрованное хранилище
  ([ADR 0008](0008-kubeconfig-vault.md)).
- CLI ищется в `PATH` login-оболочки ([ADR 0013](0013-startup.md)), затем среди копий,
  скачанных Kubach (`kubach_core::tools::cli::locate`). Свой CLI пользователя в приоритете:
  он обновляется и уже залогинен.
- Списки — вызовами CLI с JSON-выводом (`kubach-core::cloud`), каждый вызов описан данными
  (`CliCall`) и запускается через `kubach_core::process`:
  - GKE: `gcloud auth list` (есть ли активный аккаунт) → `gcloud projects list` →
    `gcloud container clusters list --project <id>` по отмеченным проектам;
  - YC: `yc config profile list` → `yc resource-manager cloud list` → `folder list
    --cloud-id` → `yc managed-kubernetes cluster list --folder-id` (с `--profile`);
  - EKS: `aws configure list-profiles`, регион профиля (`aws configure get region`),
    `aws eks list-clusters --region` по отмеченным регионам и `describe-cluster` на кластер
    (до 8 параллельно), с `--profile`, если он не `default`.
- Не залогинен — по тексту ошибки CLI (`NotLoggedIn`) показывается подсказка с командой
  входа (`gcloud auth login`, `yc init`, `aws configure` / `aws sso login --profile …`) и
  «Try again». Логин делает сам CLI в терминале; Kubach токенов облака не видит и не хранит.
- Контексты строятся так же, как это делают CLI (`kubach_core::cloud::build_kubeconfig`):
  - GKE: имена `gke_<project>_<location>_<name>`, `certificate-authority-data` из
    `masterAuth.clusterCaCertificate`, exec `gke-gcloud-auth-plugin` с
    `provideClusterInfo: true` и `installHint`; endpoint внешний, внутренний
    (`privateClusterConfig.privateEndpoint`) или DNS
    (`controlPlaneEndpointsConfig.dnsEndpointConfig.endpoint`, без CA — у него публичный
    сертификат);
  - YC: контекст `yc-<name>`, кластер и пользователь `yc-managed-k8s-<id>`, CA — PEM из
    `master.master_auth.cluster_ca_certificate` в base64, exec
    `yc k8s create-token --profile=<профиль>`; endpoint внешний или внутренний;
  - EKS: имена — ARN кластера, exec `aws --region <r> eks get-token --cluster-name <n>
    --output json` и `AWS_PROFILE`, если профиль не `default` (тот же шаблон, что в ручной
    форме, `ExecTemplate::Eks`).
  Команда exec — абсолютный путь, если CLI скачан Kubach (его нет в `PATH`), и просто имя,
  если CLI найден в `PATH` (так пишут сами CLI; переживает обновления). Для GKE из `PATH`
  плагин пишется по имени, если он тоже в `PATH`, иначе абсолютным путём рядом с gcloud.
- В ручной форме добавлен шаблон exec **Yandex Cloud** (`ExecTemplate::Yandex`).

### Загрузка CLI (исключение из ADR 0004, как для Helm в ADR 0006)
- Только по явному действию («Download …» в окне импорта или в Settings → Tools →
  Cloud CLIs), в `<data_dir>/tools/<gcloud|yc|aws>/<версия>/` через временный каталог,
  который удаляется при ошибке и отмене; старые версии удаляются после успешной загрузки.
  В систему ничего не ставится, прав администратора не нужно. Прогресс и отмена — в окне
  импорта и в Settings → Tools; там же видно, какой CLI используется, как проверена
  скачанная копия, и её можно удалить.
- Версии не закрепляются: эти CLI выходят еженедельно. Kubach берёт текущий релиз у
  производителя (`kubach_core::tools::cli::CliTool::resolve`) и проверяет тем, что
  производитель публикует:
  - **gcloud**: SHA-256 из таблицы на странице
    `docs.cloud.google.com/sdk/docs/downloads-versioned-archives` (версия — из имени
    Windows-архива на той же странице); архив — с `dl.google.com` (ссылка с другого хоста
    отвергается), разворачивается целиком (`google-cloud-sdk/bin/gcloud`). Если Google
    обновил архив раньше страницы, хэш не совпадёт и загрузка завершится ошибкой.
    Windows x64 — архив со встроенным Python; Linux-архивы тоже со своим Python; в
    macOS-архиве Python нет, нужен Python 3.10+ в системе (подсказка в окне). Windows Arm —
    нет архива с опубликованным хэшем, только подсказка. После распаковки выполняется
    `gcloud components install gke-gcloud-auth-plugin --quiet`; неудача не отменяет
    загрузку SDK, а показывается в окне с кнопкой повтора.
  - **aws**: Linux — `awscli-exe-linux-<arch>.zip` и его отсоединённая подпись `.sig`,
    проверяемая ключом AWS CLI Team из руководства AWS (зашит в Kubach,
    `kubach-core/src/tools/aws-cli-KEY.asc`, отпечаток
    `FB5D B77F D5C1 18B8 0511 ADA8 A631 0ACC 4672 475C`, истекает 2027-07-01); запускается
    `aws/dist/aws` без `install`. macOS — `AWSCLIV2.pkg`: подпись Apple Developer ID
    проверяется `pkgutil --check-signature` (команда `94KV3E626L`), пакет раскрывается
    `pkgutil --expand-full`, берётся каталог `aws-cli` из payload (без установщика). PGP
    подписи и контрольной суммы для pkg AWS не публикует. Windows — только MSI, для
    распаковки нужен `msiexec`; Kubach показывает подсказку установить AWS CLI самому.
    Версия узнаётся из `aws --version` после распаковки.
  - **yc**: версия из `storage.yandexcloud.net/yandexcloud-yc/release/stable`, архив
    `release/<v>/<os>/<arch>/yc_<v>_<os>_<arch>.tar.gz` (`.zip` на Windows) с одним бинарём.
    Yandex **не публикует контрольных сумм и подписей** (проверено по листингу бакета,
    октябрь 2026; их `install.sh` тоже ничего не проверяет), поэтому загрузку защищает только
    HTTPS; это записывается в `verification.json` и показывается («Not checked: …»).
    Windows Arm — сборки нет.
- Как проверена копия, пишется рядом (`verification.json`: SHA-256 и `published_at`,
  подписавший ключ, подписант пакета или `unchecked`).
- Механизм общий с Helm: HTTP-клиент, прокси, лимиты, временный каталог, снятие
  `com.apple.quarantine` (`kubach_core::tools::download`); распаковка всего архива —
  `tools::archive::extract_all` (пути с `..`, абсолютные и с буквой диска отвергаются,
  ссылки и спецфайлы пропускаются, общий лимит 2 ГБ). Helm работает как раньше.

## Последствия
- Новые сетевые запросы не к кластеру: страница Google, `dl.google.com`,
  `awscli.amazonaws.com`, `storage.yandexcloud.net` — только по явному действию, анонимные
  `GET` без данных пользователя ([ADR 0004](0004-local-only.md)).
- Контексты зависят от CLI: удалить скачанный CLI — контексты не смогут войти, пока CLI не
  поставят или не скачают снова (предупреждение в диалоге удаления).
- Разбор вывода CLI держится на их JSON-форматах; тесты — на записанных образцах
  (`kubach-core/src/cloud/fixtures`). Реальных входов в облака в тестах нет.
- Новый ключ AWS CLI Team (текущий истекает 2027-07-01) приходит с обновлением Kubach.
