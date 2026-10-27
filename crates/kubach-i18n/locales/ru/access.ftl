# Экраны контроля доступа: Права (что могу я, кто может) и проверки
# безопасности pod. Глаголы (get, list), ресурсы (pods, deployments.apps),
# kind RBAC (Role, ClusterRoleBinding, ServiceAccount, User, Group), группы
# API, имена полей (hostNetwork, runAsUser) и ошибки кластера — данные, не
# переводятся.

## Права

access-tab-mine = Что могу я
access-tab-who-can = Кто может
access-check-again = Проверить снова
access-pick-namespace = Выберите namespace
access-pick-namespace-description = Права проверяются для одного namespace за раз.
access-reviewing = Проверка ваших прав…
access-cant-review = Не удаётся проверить ваши права
access-noun-rules = Правила
access-noun-subjects = Субъекты
access-noun-findings = Находки
access-incomplete = Список может быть неполным. Отсутствие правила не значит, что доступ запрещён.
access-incomplete-reason = Список может быть неполным: { $error }. Отсутствие правила не значит, что доступ запрещён.
access-non-resource = Нересурсные URL: { $urls }
access-rules-note =
    { $selected ->
        [0] Правил для вас в namespace { $namespace }: { $count }, по SelfSubjectRulesReview
        [1] Правил для вас в namespace { $namespace }: { $count }, по SelfSubjectRulesReview
       *[other] Правил для вас в namespace { $namespace } (первый из выбранных: { $selected }): { $count }, по SelfSubjectRulesReview
    }
access-column-verbs = Глаголы
access-column-groups = Группы API
access-column-names = Имена
access-any-name = любые

## Кто может

access-verb-placeholder = глагол
access-resource-placeholder = ресурс, например deployments.apps
access-name-placeholder = любое имя
access-check = Проверить
access-who-can-invalid = Введите глагол и ресурс, например get pods.
access-checking = Проверка…
access-checking-description = Получаем роли и привязки кластера.
access-cant-check = Не удаётся проверить доступ
access-cant-check-description = { $error }. Чтобы получить роли и привязки, нужно право list на ресурсы rbac.authorization.k8s.io во всём кластере.
access-question-named = { $verb } { $resource } с именем { $name }
access-grants-note =
    { $selected ->
        [0] Разрешений на { $question } в namespace { $namespace }: { $count }. Только RBAC: другие авторизаторы и system:masters не показаны.
        [1] Разрешений на { $question } в namespace { $namespace }: { $count }. Только RBAC: другие авторизаторы и system:masters не показаны.
       *[other] Разрешений на { $question } в namespace { $namespace } (первый из выбранных: { $selected }): { $count }. Только RBAC: другие авторизаторы и system:masters не показаны.
    }
access-grants-note-cluster = Разрешений на { $question } во всём кластере (учитываются только ClusterRoleBinding): { $count }. Только RBAC: другие авторизаторы и system:masters не показаны.
access-nobody = RBAC никому этого не разрешает
access-view-table = Таблица
access-view-graph = Граф
access-graph-subjects = Субъекты
access-graph-bindings = Привязки
access-graph-roles = Роли
access-graph-rules = Правила
access-subject = Субъект
access-binding = Привязка
access-role = Роль
access-rule = Правило
access-rule-label = { $verbs } на { $resources }
access-rule-label-named = { $verbs } на { $resources } с именами { $names }
access-rule-groups = Группы API: { $groups }
access-open-binding = Открыть привязку
access-open-role = Открыть роль
access-open-service-account = Открыть service account

## Проверки безопасности

access-pods-not-served = Pods не поддерживаются
access-loading-pods = Загрузка pod…
access-no-access-pods = Нет доступа к pod
access-no-access-pods-description =
    { $scope ->
        [one] Ваши учётные данные не позволяют получить список pod в namespace { $namespace }.
        [many] Ваши учётные данные не позволяют получить список pod в { $namespaces } namespace.
       *[any] Ваши учётные данные не позволяют получить список pod во всех namespace.
    }
access-cant-load-pods = Не удаётся загрузить pod
access-no-findings = Находок нет
access-no-findings-description =
    { $scope ->
        [one] Ни один pod в namespace { $namespace } не нарушает проверки этого уровня.
        [many] Ни один pod в { $namespaces } namespace не нарушает проверки этого уровня.
       *[any] Ни один pod во всех namespace не нарушает проверки этого уровня.
    }
access-security-summary =
    { $scope ->
        [one] { $summary } в namespace { $namespace }: привилегированные контейнеры, hostPath, отсутствие лимитов, образы :latest и другое
        [many] { $summary } в { $namespaces } namespace: привилегированные контейнеры, hostPath, отсутствие лимитов, образы :latest и другое
       *[any] { $summary } во всех namespace: привилегированные контейнеры, hostPath, отсутствие лимитов, образы :latest и другое
    }
access-show-low = Показывать низкую важность
security-severity-high = Высокая
security-severity-medium = Средняя
security-severity-low = Низкая
security-count-high = высокая: { $count }
security-count-medium = средняя: { $count }
security-count-low = низкая: { $count }
security-no-issues = Проблем нет
security-column-severity = Важность
security-column-pod = Pod
security-column-container = Контейнер
security-column-finding = Находка
security-pod-level = pod
security-fix = Как исправить
security-open-pod = Открыть pod

## Находки (kubach-core)

security-host-namespace = Включён { $field }
security-host-network-hint = Pod видит все сетевые интерфейсы хоста. Уберите hostNetwork, если он не необходим.
security-host-pid-hint = Pod видит процессы хоста и может посылать им сигналы. Уберите hostPID, если он не необходим.
security-host-ipc-hint = Pod разделяет IPC и общую память с хостом. Уберите hostIPC, если он не необходим.
security-host-path = Том hostPath «{ $volume }»
security-host-path-hint = Монтирует { $path } с узла. Используйте вместо него PVC, configMap или emptyDir.
security-privileged = Привилегированный контейнер
security-privileged-hint = Имеет полный доступ к хосту. Задайте privileged: false.
security-privilege-escalation = Разрешено повышение привилегий
security-privilege-escalation-hint = Задайте allowPrivilegeEscalation: false.
security-runs-as-root = Работает от root
security-runs-as-root-hint = runAsUser равен 0. Используйте ненулевой UID.
security-may-run-as-root = Может работать от root
security-may-run-as-root-hint = Задайте runAsNonRoot: true или ненулевой runAsUser.
security-capability = Добавляет capability { $capability }
security-capability-hint = Уберите её, если она не необходима; лучше drop: [ALL].
security-no-limits = Нет лимитов ресурсов
security-no-limits-hint = Задайте лимиты памяти и CPU, чтобы один pod не мог отнять ресурсы узла.
security-no-memory-limit = Нет лимита памяти
security-no-memory-limit-hint = Задайте лимит памяти, чтобы один pod не мог отнять ресурсы узла.
security-no-requests = Нет запросов ресурсов
security-no-requests-hint = Задайте запросы CPU и памяти для предсказуемого планирования.
security-untagged-image = Образ без тега
security-latest-image = Образ с тегом :latest
security-mutable-tag-hint = { $image } может незаметно измениться. Закрепите тег версии или digest.
security-writable-root = Корневая файловая система доступна для записи
security-writable-root-hint = Задайте readOnlyRootFilesystem: true и явно смонтируйте пути для записи.
