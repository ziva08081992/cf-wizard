# Before/after: final corrected runtime

Measurement: perf/run-v5; immutable baseline 0.8.5 and corrected candidate0.8.6, release scratch copies, identical harness/inputs. Each process:1 warmup +9samples; P95 nearest rank=max of nine. Per-phase counters/timings, source/lock/input/binary hashes, OS peak WorkingSet/CommitCharge, sampled5ms PrivateUsage and all raw outputs are retained. Baseline semantic-refresh/rule-only feature unsupported; these rows are not equivalent speed comparisons. Native timings remain separate.

| Cohort | old median/P95 ms | new median/P95 ms | new peak WS/private MiB |
|---|---:|---:|---:|
| one-source | 5.43/5.84 | 8.85/9.72 | 6.92/1.39 |
| one-index | 5.98/7.22 | 9.34/10.83 | 8.62/2.30 |
| docs16-source | 33.29/36.55 | 44.82/47.36 | 7.27/2.62 |
| docs16-index | 36.69/39.23 | 42.91/45.51 | 9.17/3.14 |
| register-source | 14.77/17.43 | 24.53/80.48 | 7.00/1.96 |
| register-index | 44.53/70.14 | 23.21/26.81 | 8.84/2.62 |
| form-source | 50.23/54.58 | 77.65/78.03 | 8.38/3.58 |
| form-index | 122.79/152.00 | 235.28/697.84 | 10.14/4.12 |
| exclusions | 18.04/22.27 | 20.38/23.20 | 6.91/1.91 |
| successive | 12.39/12.97 | 18.46/19.02 | 7.34/2.66 |
| semantic-refresh | 19.26/36.86 | 24.98/35.23 | 8.60/2.64 |
| rule-only | 7.52/7.74 | 9.86/10.70 | 8.67/2.54 |

All staged/settings parity: True; 120 paired samples.

Candidate does more work: namespace-scoped facts/origins, frozen inventory/content proofs and independent output/settings guards. The corrected lookup intentionally restores provider.exists for missing exact inventory spellings; exact positives still reuse verified inventory. This avoids losing Windows case aliases without changing Git semantics. The earlier run-v3 optimisation which inferred absence from case-sensitive inventory is rejected; its raw results remain historical, not final performance evidence.

No end-to-end speedup is established. Stable extra filesystem/proof work costs wall time on small packages. Source-view sharing reduces repeated XML parse counts for the form (71→34 source,58→34 indexed in the measured pipeline), while metadata propagation/guards add CPU and memory. P95 outliers are observed and retained, not discarded. A small synthetic sample cannot establish bounds for real large configurations or alias fanout; the whole pipeline is not claimed linear.

Native phase is recorded in native-v086/*/result.json and work build receipts for both backends. Those real native invocations used v4; after review corrections, current emitted bytes/settings/roles are identical and current production verifier passed saved positives/negatives. No old timing/revision is relabelled. There is no before/after native speed claim. Existing databases, business runtime and real ERP configurations were not tested.

## Уточнение замера индекса

По вопросу пользователя выполнены шесть чередующихся запусков одного current measurement EXE и одного synthetic source: source,index,index,source,source,index. Каждый имеет один warmup и9samples. Все60outputs/settings совпали. Pooled median27samples: source 76.19мс, index 86.72мс. Это дополнение не заменяет и не исправляет исходный before/after run-v5.

Явное обновление индекса и открытие IndexReader находятся вне основного таймера; внутри preview/materialization и post-run cache hash. Индекс кэширует facts, а не готовый пакет. Live source proofs, чтение выдаваемых файлов и output guards сохраняются. На малом входе выигрыш extraction невелик; загрузка/проверка cached facts добавляет работу. Первоначальный form-index235мс не повторился устойчиво: новые batch medians82–91мс против source65–83мс. Существенная часть исходной разницы находилась в materialization, общем для обоих режимов; конкретная причина wall-time variability не установлена. Общий выигрыш индекса и производительность реальной большой ERP этим не доказаны.
