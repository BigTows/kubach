# События: экран Events и панель Events в деталях объекта. Причины
# (BackOff), сообщения и kind событий — данные кластера, не переводятся.

events-title = События
events-title-warnings =
    { $count ->
        [one] События · { $count } предупреждение
        [few] События · { $count } предупреждения
        [many] События · { $count } предупреждений
       *[other] События · { $count } предупреждения
    }
events-loading = Загрузка событий…
events-unavailable = События недоступны
events-none = Событий нет
events-none-description = Здесь давно ничего не происходило.
events-not-served-error = кластер не поддерживает Event API
events-not-served = Этот кластер не поддерживает Event API.
events-search-placeholder = Поиск по имени объекта…
events-warnings-only = Только предупреждения
events-no-access = Нет доступа к событиям
events-cant-load = Не удаётся загрузить события
events-summary =
    { $count ->
        [one] { $count } событие
        [few] { $count } события
        [many] { $count } событий
       *[other] { $count } события
    } · { $warnings ->
        [one] { $warnings } предупреждение
        [few] { $warnings } предупреждения
        [many] { $warnings } предупреждений
       *[other] { $warnings } предупреждения
    }
events-summary-capped =
    { $count ->
        [one] { $count } событие
        [few] { $count } события
        [many] { $count } событий
       *[other] { $count } события
    } · { $warnings ->
        [one] { $warnings } предупреждение
        [few] { $warnings } предупреждения
        [many] { $warnings } предупреждений
       *[other] { $warnings } предупреждения
    } · показаны последние { $shown }
events-noun = События
events-no-warnings = Предупреждений нет
events-new =
    { $count ->
        [one] { $count } новое событие
        [few] { $count } новых события
        [many] { $count } новых событий
       *[other] { $count } нового события
    }
events-new-tooltip = Прокрутить к новым событиям
