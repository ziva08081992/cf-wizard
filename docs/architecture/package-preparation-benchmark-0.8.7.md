# CF Wizard 0.8.7 — измерения подготовки комплекта

Одинаковый source и compiler contract, baseline0.8.6 против candidate0.8.7. Итог: 3×9 соседних AB/BA пар на режим, один warmup+один measured sample на процесс, 108 processes/216 outputs. Во время замера этот чат сборки не выполнял.

| Режим | Median baseline, мс | Median candidate, мс | Снижение | P95 baseline/candidate, мс |
|---|---:|---:|---:|---:|
| source | 78.90 | 57.09 | 27.6% | 261.47 / 145.81 |
| indexed | 83.49 | 64.88 | 22.3% | 304.69 / 153.80 |

Новая версия быстрее в26/27 соседних source pairs и23/27 indexed pairs. Median разности соседних измерений: −21,05 и−19,60мс соответственно. Все три source batch medians улучшились; indexed batch2 практически равен (145,57→145,38мс), остальные улучшились. Это синтетический результат, не SLA для ERP.

Причина: повторные Windows metadata queries. В отдельном диагностическом baseline wrappers занимали72–74% времени:8095 basic attribute queries +747 source std fallbacks +670 output std queries. Combined diagnostic сократил source fallbacks747→0 и output std670→26, сохранив8095 basic +670 ExW queries. Это counters диагностического дерева, не syscall counters финального EXE. Copy/Project read ~3,58мс не обосновывал raw-cache.

| Metadata-only loop | Доп. глубина | Baseline, мс | Candidate, мс |
|---|---:|---:|---:|
| guard | 0 | 14.0208 | 2.1546 |
| source | 0 | 0.1542 | 0.1271 |
| guard | 8 | 19.0640 | 3.0842 |
| source | 8 | 0.1995 | 0.1601 |

Guard loop проверяет один и тот же список существующих output paths; source loop — один существующий и один отсутствующий путь. Timed writes отсутствуют; вложенные пути отличаются ровно восемью каталогами. Число ancestor traversals не сокращено.

Граница основного таймера: preview/replan/materialize со строгими guards и завершающим SQLite hash; initial/explicit index update и IndexReader open исключены. Inclusive XML/kernel/reader timers нельзя складывать как exclusive CPU. Это не время GUI, Designer, ibcmd, CF или selective merge.

Первые длинные batches имели высокий разброс elapsed. Они сохранены полностью; их нельзя смешивать с итоговыми соседними парами или выбирать только удачные sample. Причина отдельных write/create-dir/scheduler выбросов не установлена. Ниже исторические для этого change preliminary measurements (1warmup+9sample/cohort), не доказательство регрессии и не итоговый speed claim.

| Preliminary cohort | Median baseline/candidate, мс | P95 baseline/candidate, мс | Peak WS baseline/candidate, MiB | Sampled private baseline/candidate, MiB |
|---|---:|---:|---:|---:|
| one-source | 8.79/7.43 | 9.83/7.95 | 6.71/7.16 | 1.54/1.80 |
| one-index | 11.91/9.05 | 943.68/10.14 | 8.45/8.50 | 2.08/2.62 |
| docs16-source | 41.47/37.95 | 43.88/43.02 | 7.13/7.19 | 2.12/2.55 |
| docs16-index | 116.09/46.55 | 158.10/73.17 | 9.12/9.06 | 3.13/3.00 |
| register-source | 23.11/25.10 | 45.42/31.77 | 6.81/6.96 | 1.98/1.95 |
| register-index | 23.34/23.63 | 27.83/73.10 | 8.73/8.84 | 2.64/2.55 |
| form-source | 130.61/163.26 | 367.08/270.52 | 8.45/8.46 | 3.37/3.41 |
| form-index | 87.29/80.31 | 320.31/121.78 | 10.16/10.04 | 4.08/3.96 |
| exclusions | 17.80/15.42 | 20.77/16.42 | 6.60/6.96 | 1.82/1.83 |
| successive | 15.85/57.45 | 18.18/59.45 | 6.79/7.43 | 2.09/2.74 |
| semantic-refresh | 19.22/30.66 | 34.83/33.99 | 8.53/8.64 | 2.58/2.64 |
| rule-only | 20.18/30.69 | 31.66/872.91 | 8.54/8.34 | 2.39/2.43 |

| Scaled preliminary cohort | Median baseline/candidate, мс |
|---|---:|
| type16 / source | 50.80/48.63 |
| type16 / indexed | 54.47/46.52 |
| form16 / source | 271.32/269.59 |
| form16 / indexed | 1141.75/282.30 |
| opaque1MiB / source | 177.04/105.45 |
| opaque1MiB / indexed | 138.39/137.22 |

696 pipeline outputs total: preliminary240 +extra240 +final216; every corresponding payload/settings equal. Финальная версия source.rs относится к216 outputs, остальная разница candidate — только стилистическая Clippy let-chain, без смены поведения. Current source/index canonical plans/reasons, staging/settings/readback roles дополнительно сверены с0.8.6.

Память в preliminary/extra receipts: OS peak working set и private commit с интервалом5мс для всего процесса, включая initial index update. Это не точное peak retained-memory только materialization. В final adjacent timing память не опрашивалась; новых persistent buffers/caches нет. Raw outputs/hashes/IO/phase/memory/ancestor counters сохранены в локальном audit cf-wizard-package-prep-optimization-20261115 (perf/run-v1, extra-v1, paired-v2; causal diagnostic astra).

Исторический замер прежней разработки50,23→77,65мс source и122,79→235,28мс indexed означал1,55× и1,92× в соответствующем режиме. Соотношение235/78≈3 сравнивает разные режимы, а не рост одной операции втрое. Follow-up уже показал нестабильность235мс; новый итоговый эксперимент проведён отдельно.

Индекс содержит facts, но проверки свежести source и output остаются обязательными. На маленьком наборе добавляется стоимость indexed facts/proofs и cache hash; он может оказаться медленнее source. В итоговом candidate64,88 против57,09мс, а не втрое. Выигрыш на больших/повторных задачах здесь не гарантируется.

Охват: Windows lexical Disk/VerbatimDisk; mapped SMB drive, valid UNC и реальные symlink недоступны/не проверены. Junction/aliases/long-path и injected errors/reparse flags проверены. Неизменённый fallback и official API contract — дополнительные основания, не подмена отсутствующих runtime controls.
