# CF Wizard 0.7.1

Мастер собирает техническую CF из XML-выгрузки основной конфигурации 1С. В Windows он открывает локальную выгрузку или файлы выбранной ветки Gitea, показывает зависимости и готовит `.cf` с настройками объединения.

Распакуйте `CF-Wizard-0.7.1-sources.zip` и выполните команды из корня распакованного проекта. Для запуска потребуется Python 3.12 x64. Для сборки CF мастер использует установленную платформу 1С.

## Подготовка и запуск

Мастер использует закреплённую версию `partial_cf.py` из репозитория alonehobo. Скачайте её с указанного коммита и проверьте SHA-256. [Исходный репозиторий](https://github.com/alonehobo/1c-partial-cf/tree/8cc8aff3705dd91100555851d398ae91a192e23d)

```powershell
py -3.12 -m venv .venv
$python = '.\.venv\Scripts\python.exe'
& $python -m pip install PySide6-Essentials==6.10.2 PySide6-Addons==6.10.2 PyInstaller==6.18.0

$helper = '.\.agents\skills\1c-partial-cf\scripts\partial_cf.py'
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $helper) | Out-Null
Invoke-WebRequest -Uri 'https://raw.githubusercontent.com/alonehobo/1c-partial-cf/8cc8aff3705dd91100555851d398ae91a192e23d/1c-partial-cf/scripts/partial_cf.py' -OutFile $helper
if ((Get-FileHash -Algorithm SHA256 -LiteralPath $helper).Hash.ToLowerInvariant() -ne 'efe67eda5f95475757efefd75c979d8144ffd7480934307db487155c87ea5699') {
    throw 'Контрольная сумма partial_cf.py отличается от закреплённой.'
}
& $python -B '.\tools\1c-delivery\cf_wizard_qt.py' --helper $helper
```

## Сборка переносимого EXE

После подготовки окружения и скрипта выполните из той же папки.

```powershell
& $python -B '.\tools\1c-delivery\build_cf_wizard_exe.py' '.\CF-Wizard-0.7.1-windows-x64.exe'
```

Окно **Настройки** содержит пути, параметры сборки и подключение к Gitea. Для Gitea укажите свой HTTPS-адрес, репозиторий, каталог выгрузки, ветку и персональный токен на чтение. Справка доступна в мастере.

Перед распространением EXE уточните условия использования встроенных компонентов. [Репозиторий скрипта](https://github.com/alonehobo/1c-partial-cf) · [Лицензии Qt for Python](https://doc.qt.io/qtforpython-6/licenses.html)
