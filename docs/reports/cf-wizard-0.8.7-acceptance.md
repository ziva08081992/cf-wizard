# CF Wizard 0.8.7 — фактическая проверка

Область: адресное устранение повторных Windows metadata queries при подготовке комплекта. Прикладное ядро readers/compiler/guards0.8.6, C01–C16 finite profile, strict XML/QName, independent goldens и semantic/cache versions сохранены. Новый код ограничен `source.rs`; Cargo.toml/lock меняют только четыре локальные версии на0.8.7. Новых crates нет.

Причина установлена диагностическим Astra экспериментом; реализацию выполнил основной агент, затем проверили fixed Sol6.1/ultra и Astra/max read-only. Requested model/effort зафиксированы; независимой runtime-квитанции actual model/effort нет, поэтому actual unknown. Итоговые receipts содержат отдельные заключения по эффективности, обоснованности и semantic Full Path.

- Rust workspace locked/offline: fmt/check/test/clippy PASS, 226tests (application5/domain13/infrastructure207/desktop1), 0failed.
- Новые tests: output prefix/error fallback, source raw2/3 boundary, file/directory reparse flags, aliases/NUL; existing junction mutation, long paths, exclusive-open и source freshness случаи сохранены. Scoped diff checked.
- Все10 fixture files и старые0.8.4/0.8.5/0.8.6 EXE SHA неизменны. Никакие независимые эталоны не обновлялись под новый результат.
- Preliminary12cohorts и scaled type16/form16/opaque1MiB:480outputs, corresponding staging/settings equal. Final production source snapshot:216outputs, equality и одинаковые source-port IO в108процессах, 3×9adjacent pairs/mode.
- Four canonical composition/reasons сравниваются с0.8.6 после исключения только timing metrics. Staging/settings/readback roles восьми historical backend cases побайтно равны.
- Текущий verifier принял10positive saved exports и отклонил10fault controls: stub UUID, full name, missing opaque payload, register full name, form query. Старые17ИБ native receipts сохраняют исходную v4 attribution; новых ИБ, native calls и business runtime в0.8.7 не было. Ошибки адаптации audit path guard/log reuse в первых служебных попытках сохранены и исправлены; это не ослабление production verifier.
- Новый EXE0.8.7: release locked/offline, отдельный portable-target, static CRT, private path remaps. AMD64 PE,9,040MiB, SHA256 `c75fc07ad394ee57ec6cad5b77a8f749dd9eac3809d92824d4c042bbf2989394`. Нет найденных частных build paths/endpoints; нет Python/Qt/VC runtime imports. WebView2 остаётся внешним runtime.
- Этот EXE прошёл13 реальных UI/IPC/preview smoke checks в изолированном профиле: загрузка/выгрузка, Russian tree, selection/uncheck/filter, preview, journal, help, comment, provider strategy/contrast/server default. Версия0.8.7 подтверждена. Native/Git-соединения не выполнялись; ручной визуальной приёмки дизайна0.8.7 нет, UI-оформление не менялось.

В итоговом synthetic form benchmark median source78,90→57,09мс (−27,6%), indexed83,49→64,88мс (−22,3%). Candidate быстрее в26/27 и23/27 соседних пар. P95 ниже; один indexed batch нейтрален. Ранние noisy серии сохранены, причина отдельных storage/scheduler выбросов не установлена. [Полная методика/cohorts/память](../architecture/package-preparation-benchmark-0.8.7.md), [решение и checkpoints](../architecture/package-preparation-performance.md).

Нельзя выводить real ERP SLA, business correctness или отсутствие любой регрессии из synthetic/native readback checks. Real symlinks не выполнены из-за1314; valid UNC, mapped SMB и фактический ACL AccessDenied не воспроизведены. Junction и injected flags/errors — дополнительный, ограниченный контроль. TOCTOU pathname checks унаследован; атомарной защиты дерева не заявлено.

Source/portable export проверяется отдельно по SOURCE-MANIFEST/SHA/CRC и локальным ссылкам; допускаются только два прежних synthetic V1 SQLite fixtures, частные inputs/indexes/ИБ/logs/cache не включаются. Опись192сторонних пакетов и сохранённые тексты лицензий неизменны. Прежние вопросы prototype origins и missing unicode-casefold root text для публичного распространения остаются открытыми. Это локальный проверяемый комплект, не публичный выпуск.

Commit/push/MR, публикация, archive/sync, новые ИБ и операции хранилища не выполнялись. Постановка сна передана другому координирующему чату по последующему поручению пользователя; этот чат таймер не создавал и сон не запускает. Будущие UI-правки: EXE/taskbar/Alt+Tab icon, native title bar theme, удаление внутреннего CF badge.
