# Подключённые файлы kubeconfig: предпросмотр, отключение, блок о
# программах и файлах kubeconfig, ошибки чтения файла и «Проверить
# подключение». Пути, имена контекстов, кластеров, пользователей и
# программ, переменные окружения и ошибки ОС — данные. kubeconfig, exec,
# auth-provider, PATH, RBAC, TLS, CA, VPN, DNS, OIDC не переводятся.

## Ошибки чтения файла

kubeconfig-file-missing = Файл не найден
kubeconfig-file-unreadable = Не удалось прочитать файл: { $error }
kubeconfig-file-invalid = Это не kubeconfig: { $error }
kubeconfig-file-locked = Хранилище заблокировано

## Отключение

kubeconfig-unlink-title = Отключить { $name }?
kubeconfig-unlink-description =
    { $count ->
        [0] Kubach перестанет читать { $path }. Контекстов из него в Kubach нет. Сам файл не меняется; подключить его снова можно в любой момент.
        [one] Kubach перестанет читать { $path }. Из Kubach пропадёт { $count } контекст: { $contexts }. Сам файл не меняется; подключить его снова можно в любой момент.
        [few] Kubach перестанет читать { $path }. Из Kubach пропадут все { $count } контекста: { $contexts }. Сам файл не меняется; подключить его снова можно в любой момент.
        [many] Kubach перестанет читать { $path }. Из Kubach пропадут все { $count } контекстов: { $contexts }. Сам файл не меняется; подключить его снова можно в любой момент.
       *[other] Kubach перестанет читать { $path }. Из Kubach пропадут все контексты ({ $count }): { $contexts }. Сам файл не меняется; подключить его снова можно в любой момент.
    }
kubeconfig-unlink = Отключить
kubeconfig-restrict-failed = Не удалось изменить права { $path }: { $error }

## Предпросмотр

kubeconfig-find-again-title = Найти файл kubeconfig заново
kubeconfig-link-title =
    { $count ->
        [one] Подключить файл kubeconfig
       *[other] Подключить файлы kubeconfig ({ $count })
    }
kubeconfig-link-description = Kubach читает файл на месте, поэтому изменения от aws, gcloud и других утилит подхватываются. Учётные данные не копируются.
kubeconfig-close = Закрыть
kubeconfig-use-file = Использовать этот файл
kubeconfig-link-file = Подключить файл
kubeconfig-link-files =
    { $count ->
        [one] Подключить { $count } файл
        [few] Подключить { $count } файла
        [many] Подключить { $count } файлов
       *[other] Подключить { $count } файла
    }
kubeconfig-permissions-warning = Этот файл могут читать другие пользователи компьютера ({ $mode }). В нём могут быть учётные данные; ограничьте права до 0600 (chmod 600 { $path }).
kubeconfig-already-linked = Этот файл уже подключён.
kubeconfig-no-contexts = В файле нет контекстов.
kubeconfig-context-shadowed = уже есть, этот пропускается
kubeconfig-no-server = Нет API-сервера
kubeconfig-contexts = Контексты ({ $count })

## Что kubeconfig запускает и читает

kubeconfig-review-programs =
    { $purpose ->
        [link] При подключении на этом компьютере запускаются эти программы. Подключайте, только если ожидаете их.
       *[import] При подключении на этом компьютере запускаются эти программы. Импортируйте, только если ожидаете их.
    }
kubeconfig-review-programs-dangerous =
    { $purpose ->
        [link] При подключении на этом компьютере запускаются эти программы. Подключайте, только если ожидаете их. Переменные, выделенные красным, позволяют программе загрузить или запустить другой код.
       *[import] При подключении на этом компьютере запускаются эти программы. Импортируйте, только если ожидаете их. Переменные, выделенные красным, позволяют программе загрузить или запустить другой код.
    }
kubeconfig-review-exec = пользователь { $user }, exec
kubeconfig-review-auth-provider = пользователь { $user }, auth-provider { $name }
kubeconfig-review-files-link = При подключении читаются эти локальные файлы; ключи и токены из них отправляются серверу кластера.
kubeconfig-review-files-import = Эти локальные файлы копируются в хранилище; ключи и токены из них отправляются серверу кластера.
kubeconfig-review-files-pasted = Вставленный конфиг ссылается на эти локальные файлы. Kubach прочитает их в хранилище, только если вы подтвердите ниже; ключи и токены из них отправляются серверу кластера.
kubeconfig-review-file-cluster = кластер { $owner }, { $key }
kubeconfig-review-file-user = пользователь { $owner }, { $key }
kubeconfig-review-file-cluster-outside = кластер { $owner }, { $key } (вне папки kubeconfig)
kubeconfig-review-file-user-outside = пользователь { $owner }, { $key } (вне папки kubeconfig)

## Ошибки «Проверить подключение»

probe-unauthorized = Ошибка аутентификации: сервер отклонил учётные данные
probe-forbidden = Вход выполнен, но читать версию сервера нельзя
probe-unknown-ca = TLS: сертификату сервера нет доверия
probe-certificate-name = TLS: сертификат сервера выдан для другого имени хоста
probe-tls = Ошибка TLS-рукопожатия
probe-dns = Имя хоста сервера не разрешается
probe-refused = В подключении отказано: по этому адресу никто не слушает
probe-timeout = Сервер не ответил вовремя
probe-exec-not-found = Команда exec-плагина не найдена
probe-exec-failed = Exec-плагин завершился с ошибкой
probe-invalid-config = Параметры подключения неверны
probe-server = Сервер ответил HTTP { $code }
probe-anonymous = Подключение есть, но сервер проигнорировал учётные данные (anonymous)
probe-other = Не удалось подключиться
probe-unauthorized-hint = Проверьте учётные данные: они могли истечь, быть отозваны или относиться к другому кластеру. Большинство кластеров (Kubernetes 1.19+) вообще не принимают имя пользователя и пароль.
probe-forbidden-hint = Учётные данные работают, но RBAC запрещает даже /version. Попросите доступ у администратора кластера.
probe-unknown-ca-hint = Укажите CA-сертификат кластера (certificate-authority-data в его kubeconfig); просроченный сертификат сервера тоже даёт эту ошибку.
probe-certificate-name-hint = Подключайтесь по имени хоста, на которое выдан сертификат, а не по IP или псевдониму.
probe-tls-hint = Проверьте CA-сертификат, клиентский сертификат и ключ, и что URL начинается с https.
probe-dns-hint = Проверьте имя хоста, а также настройки VPN или DNS.
probe-refused-hint = Проверьте порт и что кластер запущен.
probe-timeout-hint = Проверьте адрес, файрволы и VPN; exec-плагин может ждать входа.
probe-exec-not-found-hint = Установите плагин (OIDC: kubectl krew install oidc-login) или укажите полный путь; Kubach использует PATH вашей login-оболочки.
probe-exec-failed-hint = Запустите команду плагина в терминале, чтобы увидеть причину.
probe-anonymous-hint = Сервер не поддерживает такой способ входа: Kubernetes 1.19+ игнорирует имя пользователя и пароль. Используйте токен, клиентский сертификат, OIDC или exec-плагин.
