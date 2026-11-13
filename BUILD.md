# Сборка CF Wizard

Текущая версия **0.8.0-rust.2** находится в `tools/cf-wizard-rust`. Для запуска готового EXE Rust, Python и Qt не требуются; разработчику для сборки нужны Rust MSVC x64 и Microsoft C++ Build Tools. Проверенный toolchain — Rust 1.99.1. Для окна приложения нужен Microsoft Edge WebView2 Runtime, для сборки CF — установленная платформа 1С x64.

## Rust workspace

Из корня репозитория перейдите в workspace:

```powershell
Set-Location tools/cf-wizard-rust
cargo fmt --all --check
cargo check --workspace --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --release --locked
```

Результат: `target/release/cf-wizard-rust.exe` внутри workspace, либо каталог из `CARGO_TARGET_DIR`. Для распространения переименуйте копию в `CF-Wizard-Rust-0.8.0-rust.2.exe`. HTML/CSS/JavaScript встроены в EXE. `Cargo.lock` закрепляет зависимости; `.cargo/config.toml` включает статический C runtime для Windows MSVC x64.

Обычная сборка и тесты не запускают Python. Сохранённые эталоны для сопоставления с Python 0.7.3 включены в `tests/fixtures`; Python-генераторы нужны только для их повторного получения.

Состав слоёв и детали запуска: [README Rust-версии](tools/cf-wizard-rust/README.md). Границы проверок rust.2: [описание выпуска](docs/releases/v0.8.0-rust.2.md). Успешные unit-тесты и обратная выгрузка не подтверждают совместимость объединения с целевой информационной базой.

## Обновление инструкции Word

Источник текста — `docs/user-guide.md`. Актуальные реальные скриншоты — `docs/images/rust-0.8.0-rust.2`. Для подготовки Word нужен отдельный Python с `python-docx`:

```powershell
python tools/docs/build_user_guide.py
```

Результат — `docs/CF-Wizard-0.8.0-rust.2-User-Guide-RU-Illustrated.docx`. Параметры `--source`, `--output` и `--version` позволяют собрать другую редакцию без изменения генератора. Комментарий `<!-- pagebreak -->` в Markdown задаёт начало страницы в Word.

Перед публикацией откройте Word, обновите поля и проверьте каждую страницу, таблицу и подпись. Допустим рендеринг через Word либо LibreOffice с просмотром всех страниц. Внешние ссылки в Word преобразуются в адреса GitHub, поэтому документ можно читать отдельно от репозитория.

Иллюстрированный Word имеет отдельное имя: он не подменяет текстовую инструкцию и `SHA256SUMS.txt`, опубликованные в Assets тега `v0.8`.

## Прежняя Python-версия

Код `tools/1c-delivery` и старые архивы сохранены для истории и сравнительных проверок. Они не входят в исполняемый Rust EXE. Инструкции по старой сборке доступны в [BUILD.md версии 0.7.2](https://github.com/ziva08081992/cf-wizard/blob/v0.7.2/BUILD.md).
