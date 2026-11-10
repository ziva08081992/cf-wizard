"""Russian display labels for Designer metadata identifiers."""

TYPE_LABELS = {
    "Subsystem": ("Подсистемы", "Подсистема"),
    "CommonModule": ("Общие модули", "Общий модуль"),
    "SessionParameter": ("Параметры сеанса", "Параметр сеанса"),
    "Role": ("Роли", "Роль"),
    "CommonAttribute": ("Общие реквизиты", "Общий реквизит"),
    "ExchangePlan": ("Планы обмена", "План обмена"),
    "FilterCriterion": ("Критерии отбора", "Критерий отбора"),
    "EventSubscription": ("Подписки на события", "Подписка на событие"),
    "ScheduledJob": ("Регламентные задания", "Регламентное задание"),
    "FunctionalOption": ("Функциональные опции", "Функциональная опция"),
    "FunctionalOptionsParameter": ("Параметры функциональных опций", "Параметр функциональной опции"),
    "DefinedType": ("Определяемые типы", "Определяемый тип"),
    "SettingsStorage": ("Хранилища настроек", "Хранилище настроек"),
    "CommonForm": ("Общие формы", "Общая форма"),
    "CommonCommand": ("Общие команды", "Общая команда"),
    "CommandGroup": ("Группы команд", "Группа команд"),
    "CommonTemplate": ("Общие макеты", "Общий макет"),
    "CommonPicture": ("Общие картинки", "Общая картинка"),
    "XDTOPackage": ("Пакеты XDTO", "Пакет XDTO"),
    "WebService": ("Web-сервисы", "Web-сервис"),
    "HTTPService": ("HTTP-сервисы", "HTTP-сервис"),
    "WSReference": ("WS-ссылки", "WS-ссылка"),
    "IntegrationService": ("Сервисы интеграции", "Сервис интеграции"),
    "Bot": ("Боты", "Бот"),
    "StyleItem": ("Элементы стиля", "Элемент стиля"),
    "Style": ("Стили", "Стиль"),
    "PaletteColor": ("Цвета палитры", "Цвет палитры"),
    "Interface": ("Интерфейсы", "Интерфейс"),
    "Language": ("Языки", "Язык"),
    "Constant": ("Константы", "Константа"),
    "Catalog": ("Справочники", "Справочник"),
    "Document": ("Документы", "Документ"),
    "DocumentNumerator": ("Нумераторы", "Нумератор"),
    "Sequence": ("Последовательности", "Последовательность"),
    "DocumentJournal": ("Журналы документов", "Журнал документов"),
    "Enum": ("Перечисления", "Перечисление"),
    "Report": ("Отчёты", "Отчёт"),
    "DataProcessor": ("Обработки", "Обработка"),
    "ChartOfCharacteristicTypes": ("Планы видов характеристик", "План видов характеристик"),
    "ChartOfAccounts": ("Планы счетов", "План счетов"),
    "ChartOfCalculationTypes": ("Планы видов расчёта", "План видов расчёта"),
    "InformationRegister": ("Регистры сведений", "Регистр сведений"),
    "AccumulationRegister": ("Регистры накопления", "Регистр накопления"),
    "AccountingRegister": ("Регистры бухгалтерии", "Регистр бухгалтерии"),
    "CalculationRegister": ("Регистры расчёта", "Регистр расчёта"),
    "BusinessProcess": ("Бизнес-процессы", "Бизнес-процесс"),
    "Task": ("Задачи", "Задача"),
    "ExternalDataSource": ("Внешние источники данных", "Внешний источник данных"),
}


def group_label(kind: str) -> str:
    return TYPE_LABELS.get(kind, (kind, kind))[0]


def object_label(key: str) -> str:
    kind, sep, name = key.partition(".")
    return f"{TYPE_LABELS[kind][1]}.{name}" if sep and kind in TYPE_LABELS else key


def object_groups(objects: tuple[str, ...], search: str) -> dict[str, list[str]]:
    query = search.strip().casefold().replace("ё", "е")
    groups = {}
    for key in objects:
        kind = key.partition(".")[0]
        labels = f"{key} {object_label(key)} {group_label(kind)}".casefold().replace("ё", "е")
        if query in labels:
            groups.setdefault(kind, []).append(key)
    ordered = (*TYPE_LABELS, *sorted(groups.keys() - TYPE_LABELS.keys()))
    return {kind: groups[kind] for kind in ordered if kind in groups}
