# Замеры расчёта 0.8.4 и 0.8.5

Изолированные release-драйверы, одинаковые входы, один прогрев и девять измерений. Время — миллисекунды. Это не запуск EXE/интерфейса и не полная конфигурация. Новые missing-диагностики и исправленные пути меняют объём полезной работы; время отказа materialization не сравнивается с успешной сборкой.

| Случай | SQLite | Preview 0.8.4 median/P95 | Preview 0.8.5 median/P95 |
| --- | --- | --- | --- |
| T02_long_supported_form_path | нет | 7.411 / 20.140 | 12.198 / 17.510 |
| T02_long_supported_form_path | да | 7.371 / 20.523 | 11.461 / 15.933 |
| T09_missing_structured_object | нет | 1.288 / 3.792 | 1.531 / 2.028 |
| T09_missing_structured_object | да | 1.172 / 3.498 | 1.438 / 4.456 |
| T09_missing_explicit_member | нет | 1.090 / 4.231 | 1.532 / 4.949 |
| T09_missing_explicit_member | да | 1.177 / 4.094 | 1.667 / 4.390 |
| T10_plain_text_is_not_mandatory | нет | 0.866 / 2.635 | 1.018 / 2.815 |
| T10_plain_text_is_not_mandatory | да | 0.921 / 2.785 | 0.870 / 2.730 |
| T04_existing_dimension_satisfies_minimum | нет | 1.270 / 3.533 | 1.515 / 4.781 |
| T04_existing_dimension_satisfies_minimum | да | 1.212 / 5.160 | 1.588 / 4.680 |
| T04_resource_priority_is_explicit | нет | 1.305 / 4.455 | 1.649 / 4.443 |
| T04_resource_priority_is_explicit | да | 1.346 / 3.538 | 1.759 / 4.407 |
| T04_no_structural_candidate | нет | 1.494 / 3.566 | 1.870 / 4.370 |
| T04_no_structural_candidate | да | 1.146 / 3.355 | 1.496 / 4.329 |
| T05_prefer_included_doc_and_keep_exact_link | нет | 2.258 / 5.425 | 3.241 / 7.627 |
| T05_prefer_included_doc_and_keep_exact_link | да | 2.080 / 4.544 | 2.891 / 7.616 |
| T05_doc_as_type_does_not_add_movements | нет | 1.358 / 2.893 | 1.591 / 4.356 |
| T05_doc_as_type_does_not_add_movements | да | 1.365 / 2.155 | 1.444 / 4.057 |

Нового общего ускорения на этих маленьких примерах не подтверждено. Дополнительная стоимость включает нормализацию/проверки/координатор/причины и работу исправленных ветвей. В частности T02 теперь обходит длинный путь, который прежняя реализация обрезала. Adapter_ms включает IO, parsing и пробную проекцию; это не чистое время парсера.

| Процесс-когорта | Пиковый WorkingSet, медиана МиБ | PrivateCommit sampled, медиана МиБ |
| --- | --- | --- |
| product/baseline | 7.523 | 2.527 |
| product/datafrog | 8.789 | 3.180 |
| legacy/baseline | 9.633 | 4.105 |
| legacy/datafrog | 12.340 | 6.602 |

Память относится к целому драйверу и сохранённым отчётам когорты. Сэмплированный PrivateCommit отличается от накопительного системного PeakCommit. Портовые счётчики не видят внутренний IO LocalSource::snapshot. Для нового драйвера проверяется побайтовая неизменность индекса до/после preview.

Модельное ядро отдельно проверено на 57 случаях: один прогрев и девять measured queries, solve/provenance/setup разделены. Его PeakCommit — PeakPagefileUsage процесса, накопительный по повторам. Эти модельные цифры нельзя переносить на XML, SQLite, native или UI.

Новый механизм agenda ограничивает нормализации 20/100 независимых регистров величиной 5N. Это контроль сложности, а не массовый wall-time benchmark. Из raw registrar cache убраны посторонние документы; снижение памяти на полной ERP-выгрузке в этой работе не измерялось.
