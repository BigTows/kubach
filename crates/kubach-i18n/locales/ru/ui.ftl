# Встроенные тексты компонентов дизайн-системы (kubach-ui). Имена и
# значения от приложения (kind, состояния, имена контекстов) — данные.

## Общее

ui-cancel = Отмена
ui-and-more = и ещё { $count }
ui-unavailable = нет данных

## Тост действия

ui-action-stop = Остановить
ui-action-stopping = Остановка…

## Массовое действие

ui-bulk-running = Выполняется…
ui-bulk-done = Готово
ui-bulk-cancelled = Отменено
ui-bulk-failed = Ошибка
ui-bulk-succeeded = Успешно: { $succeeded }
ui-bulk-succeeded-failed = Успешно: { $succeeded }, с ошибкой: { $failed }
ui-bulk-hide-tooltip = Скрыть; выполнение продолжится в статус-баре
ui-bulk-hide-output = Скрыть вывод
ui-bulk-show-output = Показать вывод ({ $count })

## Чипы (labels и annotations)

ui-chip-click-to-copy = Нажмите, чтобы скопировать
ui-chip-hide = Скрыть
ui-chip-show-less = Свернуть
ui-chip-show-all = Показать все ({ $count })
ui-chip-more = ещё { $count }
ui-chip-copy-all = Скопировать все

## Диалог подтверждения

ui-confirm = Подтвердить
ui-confirm-command = Команда
ui-confirm-copy-command = Скопировать команду
ui-confirm-type-to-confirm = Для подтверждения введите { $text }.
ui-confirm-context = Контекст

## Diff

ui-diff-unchanged-lines =
    { $count ->
        [one] ⋯ { $count } строка без изменений
        [few] ⋯ { $count } строки без изменений
        [many] ⋯ { $count } строк без изменений
       *[other] ⋯ { $count } строки без изменений
    }

## Поиск в списках

ui-filter-no-matches = { $noun }: ничего не найдено по запросу «{ $query }»
ui-filter-no-matches-description = Поиск ищет текст в любом месте имени без учёта регистра.
ui-filter-clear-search = Сбросить поиск

## Граф трафика

ui-traffic-low = Низкий
ui-traffic-medium = Средний
ui-traffic-high = Высокий
ui-traffic-degraded = Ошибки ≥1%
ui-traffic-failing = Ошибки ≥5%
ui-traffic-no-service = Без Service
ui-graph-group-items =
    { $count ->
        [one] { $count } объект
        [few] { $count } объекта
        [many] { $count } объектов
       *[other] { $count } объекта
    }
ui-graph-zoom-out = Уменьшить
ui-graph-actual-size = Реальный размер
ui-graph-zoom-in = Увеличить
ui-graph-fit = Вписать в окно
ui-graph-minimap = Мини-карта
ui-graph-laying-out = Раскладка…
ui-graph-click-to-pan = Щёлкните, чтобы двигать и масштабировать

## Цвета плиток хотбара

ui-tile-blue = Синий
ui-tile-teal = Бирюзовый
ui-tile-orange = Оранжевый
ui-tile-purple = Фиолетовый
ui-tile-pink = Розовый
ui-tile-gray = Серый
ui-tile-custom = Свой

## Графики

ui-chart-now = сейчас
ui-chart-no-data = Нет данных

## Логи

ui-log-no-lines = Строк пока нет
ui-log-new-lines =
    { $count ->
        [one] { $count } новая строка
        [few] { $count } новые строки
        [many] { $count } новых строк
       *[other] { $count } новой строки
    }

## Множественный выбор

ui-multi-none = Ничего
ui-multi-filter = Фильтр…
ui-multi-all = Все
ui-multi-select = Выбрать…
ui-multi-recent = Недавние
ui-multi-use = Добавить «{ $value }»
ui-multi-no-matches = Нет совпадений
ui-multi-hint = Применяется сразу · ⌥-клик — только один

## Операции

ui-operations-title = Операции
ui-operations-empty = Операций пока нет
ui-operations-empty-description = Здесь видны изменения Helm, drain и массовые действия, пока они выполняются.
ui-operations-clear-finished = Убрать завершённые
ui-operations-notice-tooltip = Показать операции

## Таблицы и полосы

ui-replicas-ready = готово { $ready }/{ $desired }
ui-table-count-age = { $count } ({ $age } назад)
ui-table-reset-columns = Сбросить порядок колонок
ui-cubes-restarts = Перезапуски: { $count }
ui-side-list-empty = Здесь пусто
ui-usage-allocatable = Allocatable
ui-usage-used = Использовано
ui-usage-requests = Requests
ui-usage-limits = Limits

## Поля секретов и настроек

ui-secret-choose-file = Выбрать файл…
ui-secret-no-file = Файл не выбран
ui-secret-length = Длина
ui-secret-generate = Сгенерировать
ui-setting-reset = Сбросить к значению по умолчанию
ui-setting-not-set = Не задано
ui-setting-choose = Выбрать…
