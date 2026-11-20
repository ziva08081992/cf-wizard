# Диагностика полного preview

Диагностическая версия `0.8.9-diagnostics.1` измеряет прежний алгоритм 0.8.8. Оптимизация чтений, смена cache policy, перестроение индекса и новая отмена SQLite в неё не входят. Форматы SourceFacts/индекса и внешние зависимости сохраняются.

## Запуск на RDP

1. Распаковать отдельную поставку в новую папку, сохранив 0.8.8. EXE, `Start-ProfilePreview.cmd` и `Start-ProfilePreview.ps1` должны лежать рядом.
2. Запустить `Start-ProfilePreview.cmd` либо PowerShell launcher. Они задают `CF_WIZARD_PROFILE=1` только дочернему процессу и не меняют глобальное окружение. Прямой запуск EXE оставляет диагностику выключенной, если переменная не задана родительским процессом.
3. Использовать прежние источник Git, ревизию, индекс, кэш и 10 выбранных корней. Не удалять кэш и не перестраивать индекс ради замера. Папка журнала остаётся указанной в настройках мастера.
4. Открыть источник, дождаться проверки готовности и выполнить два последовательных preview. Записать, что происходило перед первым запуском; первый запуск сам по себе не означает cold cache.
5. В журнале проверить `profile-start`: version, build_id и schema=1. Должны появиться отдельные операции readiness и preview со своими operation_id и завершающими `profile-summary`/`profile-end`. Если менять папку индекса отдельно, её проверка при включённой диагностике пишет отдельный `index-readiness` журнал.
6. Для анализа передать новые `profile-*` блоки вместе с прежними `preview-index-open`, `preview-engine` и итоговым статусом. Старый `dependencies-ready` содержит имена объектов и полный состав; он не обезличивается этим изменением. Новые события не содержат путей, корпоративных имён или текстов исходников.

Выбор сравнивается по selection_fingerprint и selected_count; Git input — по pinned_revision, input_fingerprint и index_fingerprint, когда они фактически вычислены. Отсутствующий fingerprint не считается совпавшим. При отличии сборки или входа прогон обозначается новым случаем, а не точным AB-сравнением с историческими 446,7 с. SHA256 самого EXE проверяется по файлу поставки; приложение не перечитывает EXE для каждого preview.

## Контракт измерений

`cf-application::profile` содержит optional DTO, bounded collector и lexical scope. При выключенном режиме не запускаются таймеры/CPU API и не сохраняются unique keys. Infrastructure оборачивает существующие операции. Desktop сериализует результаты best effort; ошибка дополнительной диагностики не заменяет результат preview. Readiness отдельно записывает уже полученные index_ready/index_found; успешная проверка с ready=false не выдаётся за доступный индекс. Существующие события и ошибки их записи сохраняют прежнее поведение.

Worker-потоки явно получают копию operation context и текущего parent. Глобальный mutable observer не переключается. Git SourceCounters монотонны для lifetime источника; before/after snapshots никогда не сбрасывают общий счётчик. `source_counters_available=false` явно обозначает отсутствие/недоступность lifetime source counters; это не нулевой замер. `shared_source_activity=true` отмечает посторонние started/completed calls, активность на начальной границе либо чужие активные spans на конечной. Собственные active/started counts вычитаются по тому же SourceCounters. При sticky overflow/truncation delta обозначается source_delta_lower_bound; нулевой delta тогда не доказывает отсутствие работы. Счётчики не сбрасываются. Activity scope — I/O методы MetadataSource, включая полностью тёплое inventory window и весь prefetch; process CPU имеет более широкий scope.

Все длительности — наносекунды монотонного `Instant`; для миллисекунд разделить на 1 000 000. `inclusive_ns` — включающая сумма вызовов. У каждого агрегата есть semantic group, фактические parents и признак inclusive; если parents различались, parent="multiple". Вложенные и параллельные суммы не складываются как elapsed.

| Поля | Семантика |
|---|---|
| calls / errors | Выполненные вызовы, в том числе ошибочные; диагностические вызовы не дедуплицируют исполнение |
| bytes | Логические байты успешно возвращённого payload либо входные байты реально выполненной проверки/parse; не physical disk или compressed wire bytes |
| items | Число элементов/событий, указанное именем метрики: manifest rows, stale rows, selected_count, replay generation paths, scan byte counters |
| unique | Число различных SHA256 ключей файла/владельца в данной категории; сами ключи в журнал не попадают |
| unique_lower_bound | Набор ключей насыщен; unique — только нижняя граница |
| maximum_combined_in_flight / final_in_flight | Одновременно выполняемые логические tree/file provider calls и остаток после существующего join; HTTP вызовы учитываются отдельно |
| cpu / cpu_samples | Доступная сумма user/kernel process CPU на coarse boundaries и число измеренных вызовов; CPU не является wall time |
| engine_partition_ns / engine_unattributed_ns | Сумма непересекающихся верхних фаз и оставшееся время engine.total; вложенные cache/hash/decode суммы повторно не вычитаются |

`wall_ns` завершается до сериализации и записи summary; `profile-end.summary_prepare_and_write_ns` показывает стоимость подготовки/записи. Полное ожидание пользователя остаётся в существующих started/finished. Предел overhead проверяется отдельно сравнением baseline/candidate, включая запись новых событий.

## Каталог стадий

- Индекс: `index.candidate_open/connect/busy_timeout/begin/application_id`, `index.connect`, `index.begin`, `index.application_id`, `index.quick_check`, `index.foreign_key_check`, `index.header`, `index.semantic_objects`, `index.semantic_documents`, `index.manifest_rows`, `index.fingerprint.readback/stored/current`, `index.complete`. Ошибка подэтапа отмечается errors; последующие невыполненные стадии отсутствуют. В `index.open` нет новой interrupt/progress cancellation.
- Engine: `engine.setup`, `engine.selected_scan`, `engine.resolve`, `engine.closure`, `engine.normalize`, `engine.projection_compile`, `engine.form_replay`, `engine.finish_inventory`, `engine.omitted_module`, `engine.result_prepare`, `engine.package_compile`. Полный inventory, source proofs, XML/Git проверки и combined budget ≤ 4 сохраняются.
- Чтение: `source.read/exists/list_directory/files_below/snapshot/inventory_window/prefetch`, `source.fingerprint`, `cache.read/write`, `cache.file_hit/miss`, `cache.tree_path_hit/tree_oid_hit`, `provider.tree/file`, `http.response_body`. Provider method может пользоваться собственным кэшем; фактический transport отражается HTTP-метрикой. Байты неуспешного частичного HTTP/cache чтения не заявляются как полностью измеренные.
- Проверки: `hash.git_oid`, `hash.sha256.content`, `engine.record_hash`; это CPU/логическое чтение, а не доказанная задержка сети. SHA256 coverage частичная: engine и source_reader content hashes; остальные manifest/proof/package hashes входят в собственные enclosing spans, а не в эту строку. На здоровом полностью тёплом file cache одно Source.read сохраняет прежние три логических чтения и четыре Git OID-проверки.
- SourceFacts: `facts.lookup`, `facts.decode`, `facts.inflate`, `facts.inflated_bytes`, `facts.json`, `facts.validate.codec`, `facts.validation_in_structural`, `facts.structural*`, `document.cached/descriptor/extract`. Путь document → facts может декодировать одного владельца повторно; calls/unique показывают повтор без его устранения. facts.validate.codec измеряет существующую codec validation. facts.validation_in_structural — счётчик обязательной validation внутри существующего domain structural_facts; время включено в facts.structural*, отдельного validate прохода нет. SourceDocument.cached не вызывает полный validate, и фиктивная метрика для него не создаётся.
- Сканы: `scan.file_bsl/form/xml/opaque`, `engine.scan`, `xml.parse/spans`, `bsl.literal_scan/comment_scan`, `form.read/requirements`, `form.replay_generation`, `scan.xml_first/repeat`. Пересчёт формы из-за изменения generation остаётся частью closure.

## CPU и лимиты

CPU снимается только на уровне операции и coarse phases: весь Rust-процесс со всеми worker threads, включая возможную работу UI; дочерний WebView не входит. Windows adapter использует [GetProcessTimes](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-getprocesstimes) и [GetCurrentProcess](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-getcurrentprocess), FILETIME переводится из 100 ns. При недоступном API/платформе значение null с availability reason; основной сценарий продолжается.

На категорию хранится не более 32 768 digests, на операцию — не более 128 категорий, 16 parents на категорию и 16 identity полей по 128 байт. При ограничении parents помечается parents_complete=false. При насыщении полные calls/bytes продолжаются, unique становится lower bound. Переполнение счётчиков насыщается без panic и помечается counter_overflow.

Дополнительный вывод ограничен 64 KiB UTF-8: summary ≤ 60 KiB, 4 KiB зарезервированы для start/end и JSONL envelopes. При сокращении summary сохраняются operation/status и `truncated`, `truncation_reason`, omitted_metric_rows. Лимит относится только к новым событиям, не к старому полному dependencies-ready. Нет per-file логирования или дополнительных SQL/source reads для статистики.

## Проверка и границы вывода

Независимые oracle fixtures проверяют результат off/on; contract tests проверяют повторные чтения/decode, invalid index, ошибки и join. N64 стенд сравнивает 27 чередующихся AB/BA пар warm/0, warm/20 и cold/20. IndexReader открыт внутри profiling context; полная operation boundary включает index + engine + summary/write, а index и engine дополнительно имеют собственные component gates; допустимая прибавка median — max(2 ms, 10%) baseline. Сохраняются все samples, p95, parity и control cold/0. Синтетический результат и UI smoke не подтверждают причину задержки ERP или функциональность 1С. Окончательная RCA требует реального RDP trace.

Diagnostic profile events retain every schema field, including explicit zero, null and false values. Their JSONL envelope is serialized into one bounded buffer before a single compressed write and flush; established non-profile journal writes remain unchanged. The size computed for the summary budget is reused by profile-end.
