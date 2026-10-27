## Notes of the Diagnostics and Security sections

details-note-title = { $severity }: { $title }
details-note-hint = Подсказка: { $hint }
diagnostic-severity-error = Ошибка
diagnostic-severity-warning = Предупреждение
diagnostic-severity-info = Инфо


## Pod diagnostics (kubach-core/src/diagnostics.rs)

diagnostic-container = Контейнер `{ $name }`
diagnostic-init-container = Init-контейнер `{ $name }`
diagnostic-restarts =
    { $count ->
        [one] { $count } перезапуск
        [few] { $count } перезапуска
        [many] { $count } перезапусков
       *[other] { $count } перезапуска
    }
diagnostic-exit-code = код выхода { $code }
diagnostic-exit-reason = { $reason }, код выхода { $code }
diagnostic-last-run = Последний запуск завершился с кодом { $code }.
diagnostic-last-run-signal = Последний запуск завершился с кодом { $code } ({ $signal }).
diagnostic-last-run-reason = Последний запуск завершился с кодом { $code }, причина: { $reason }.
diagnostic-last-run-signal-reason = Последний запуск завершился с кодом { $code } ({ $signal }), причина: { $reason }.

diagnostic-terminating = Pod завершается
diagnostic-terminating-detail = Запрошено удаление; период ожидания закончится через { $duration }.
diagnostic-stuck = Pod завис при завершении: { $duration }
diagnostic-stuck-detail = Период ожидания закончился { $duration } назад, но Pod всё ещё существует.
diagnostic-stuck-finalizers-detail = Период ожидания закончился { $duration } назад; остались финализаторы: { $finalizers }.
diagnostic-stuck-hint = Проверьте, что узел доступен и kubelet работает; принудительное удаление — крайняя мера
diagnostic-stuck-finalizers-hint = Проверьте контроллеры, которым принадлежат финализаторы, или удалите финализаторы, если этих контроллеров уже нет

diagnostic-evicted = Pod вытеснен
diagnostic-evicted-hint = На узле не хватило ресурсов; задайте requests по фактическому потреблению
diagnostic-pod-failed = Pod завершился с ошибкой: { $reason }

diagnostic-gated = Pod ждёт снятия scheduling gates
diagnostic-gated-hint = Проверьте spec.schedulingGates и контроллер, который ими управляет
diagnostic-unschedulable = Pod не удаётся разместить
diagnostic-unschedulable-causes = Pod не удаётся разместить: { $causes }
diagnostic-unschedulable-hint = Прочитайте сообщение планировщика и сравните спецификацию Pod с доступными узлами
diagnostic-cause-cpu = не хватает CPU
diagnostic-cause-memory = не хватает памяти
diagnostic-cause-resource = не хватает { $resource }
diagnostic-cause-taints = taints без tolerations
diagnostic-cause-selector = ни один узел не подходит под selector/affinity
diagnostic-cause-pod-affinity = не выполнены правила pod affinity
diagnostic-cause-pvc = PersistentVolumeClaim не привязан
diagnostic-cause-volume-affinity = конфликт node affinity тома
diagnostic-cause-ports = порты узла заняты
diagnostic-resources-hint = Уменьшите requests контейнеров или добавьте мощности в кластер
diagnostic-taints-hint = Добавьте подходящие tolerations или размещайте на узлах без этих taints
diagnostic-selector-hint = Сверьте nodeSelector/affinity с метками узлов
diagnostic-pod-affinity-hint = Сверьте правила podAffinity/podAntiAffinity с запущенными Pod
diagnostic-pvc-hint = Проверьте, что PVC существует и его StorageClass может создать том
diagnostic-volume-affinity-hint = Привязанный том находится в зоне или на узле, куда Pod нельзя разместить
diagnostic-ports-hint = Уберите hostPort или освободите порт на узлах

diagnostic-crash-looping = { $container } падает в цикле ({ $restarts })
diagnostic-crash-looping-exit = { $container } падает в цикле ({ $exit }, { $restarts })
diagnostic-previous-logs-hint = Посмотрите логи предыдущего запуска
diagnostic-image-pull = { $container } не может скачать образ `{ $image }` ({ $reason })
diagnostic-image-name-hint = Исправьте ссылку на образ в спецификации Pod
diagnostic-never-pull-hint = imagePullPolicy равен Never, но образа нет на узле
diagnostic-credentials-hint = Проверьте imagePullSecrets и учётные данные реестра
diagnostic-image-missing-hint = Проверьте, что образ с таким именем и тегом есть в реестре
diagnostic-image-hint = Проверьте имя и тег образа и доступность реестра с узла
diagnostic-missing-configmap = { $container } ссылается на отсутствующий ConfigMap `{ $name }`
diagnostic-missing-secret = { $container } ссылается на отсутствующий Secret `{ $name }`
diagnostic-invalid-config = { $container }: некорректная конфигурация
diagnostic-config-hint = Создайте нужный ConfigMap/Secret или исправьте ссылки в env/envFrom
diagnostic-waiting = { $container } ожидает: { $reason }
diagnostic-failed = { $container } завершился с ошибкой ({ $exit })
diagnostic-logs-hint = Посмотрите логи контейнера
diagnostic-restarted = { $container } перезапускался ({ $exit }, { $restarts })

diagnostic-oom = { $container } завершён по OOMKilled ({ $restarts })
diagnostic-oom-looping = { $container } завершён по OOMKilled ({ $restarts }, падает в цикле)
diagnostic-oom-limit = { $container } завершён по OOMKilled (лимит { $limit }, { $restarts })
diagnostic-oom-limit-looping = { $container } завершён по OOMKilled (лимит { $limit }, { $restarts }, падает в цикле)
diagnostic-oom-limit-detail = Контейнер превысил лимит памяти { $limit } и был завершён.
diagnostic-oom-limit-hint = Поднимите лимит памяти выше { $limit } или уменьшите потребление памяти
diagnostic-oom-detail = Контейнер завершён из-за чрезмерного потребления памяти; лимита памяти у него нет, поэтому память закончилась на самом узле.
diagnostic-oom-hint = Задайте requests/limits памяти по фактическому потреблению контейнера

diagnostic-startup-probe = { $container } не прошёл startup probe
diagnostic-startup-probe-hint = Проверьте startup probe и логи контейнера
diagnostic-not-ready = { $container } работает, но не готов
diagnostic-not-ready-hint = Проверьте readiness probe и логи контейнера
diagnostic-pod-not-ready = Pod работает, но не готов
diagnostic-pod-not-ready-hint = Проверьте spec.readinessGates и условия Pod

diagnostic-volume-mount = Не удаётся смонтировать том
diagnostic-volume-configmap-hint = Создайте ConfigMap `{ $name }` или исправьте ссылку в томе
diagnostic-volume-secret-hint = Создайте Secret `{ $name }` или исправьте ссылку в томе
diagnostic-volume-hint = Проверьте том, его PVC и драйвер хранилища
diagnostic-sandbox = Не удаётся создать sandbox для Pod
diagnostic-sandbox-hint = Проверьте CNI-плагин и среду выполнения контейнеров на узле


## Phrases of detail fields with data in them

details-joined = { $first } · { $second }
gateway-listener-port = { $protocol } :{ $port }
gateway-listener-host = хост { $host }
gateway-listener-tls = TLS { $mode }
gateway-listener-routes-from = маршруты из { $from }
gateway-listener-routes-attached =
    { $count ->
        [one] { $count } маршрут подключён
        [few] { $count } маршрута подключено
        [many] { $count } маршрутов подключено
       *[other] { $count } маршрута подключено
    }
crd-show-objects = Показать { $plural }
crd-version-notes = { $version } ({ $notes })
crd-version-storage = хранение
crd-version-not-served = не обслуживается
crd-version-deprecated = устарела
namespace-condition-hint = { $message } — { $hint }
namespace-finalizers-hint = контроллеры, которым принадлежат эти финализаторы, должны их снять (или их удаляют вручную)
namespace-discovery-hint = агрегированный API (часто metrics.k8s.io) не отвечает; исправьте или удалите его APIService
