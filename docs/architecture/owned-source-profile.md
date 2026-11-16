# Собственные читатели и выдача: 0.8.7

Документ описывает прикладной профиль, введённый в 0.8.6 и сохранённый в локальной версии 0.8.7. Новый EXE, итоговые ревью и проверки описаны в [приёмке 0.8.7](../reports/cf-wizard-0.8.7-acceptance.md). Native/merge receipts относятся к прежним ревизиям и перенесены по точному соответствию bytes/settings/roles; в 0.8.7 платформа не запускалась. Успешный unit/model case не является платформенной или пользовательской приемкой.

## Поток и владельцы

```text
source + совместимые V2 facts + source proof
    -> SourceDocument / исходные typed facts и lexical hints
    -> Requirements / Datafrog / structural coordinator
    -> final composition
    -> frozen package projection + merge plan + единый proof
    -> byte emission + независимые file/XML/settings guards
    -> readback roles -> native/readback -> CF + merge settings
```

При рабочем расчете не формируется промежуточная заглушка для поиска новых зависимостей. Проектор и сериализатор не выбирают новые поля, регистраторов или защиты. Полное содержимое, выбранный объект/зависимость/служебная роль и действие объединения учитываются отдельно: полный CommonPicture/Style dependency сохраняет приоритет получателя.

| Карта | Реализация / граница |
|---|---|
| C01–C02 | `source_xml`, `source_reader`, `source_schema`, `reference_lexer`: namespace-scoped metadata/type/property/item facts, происхождение фрагментов и отдельные strength/necessity/missing policy |
| C03 | `kinds`: конечный совместимый словарь и policy tables. Значения и их историческое происхождение сохранены; независимые source/model cases проверяют каждый активный row/branch. `REQUIRED_*` являются политикой инструмента, не доказательством универсальной минимальности платформы |
| C04–C06 | `engine` и `engine_normalize`: адаптер source facts, finite path policy, source-order кандидаты, minimum/master/property-item/exact registrar link choices. Совместимые правила сохраняются, старый metadata producer не вызывается |
| C07 | `source_forms`: общий проверенный XML view, Attributes/Columns/MainTable/QueryText/DataPath, связь Items.CurrentData с фактическим XML-владельцем. Form/Template declarations не являются полями объекта |
| C08 | `source_query` и `source_literals`: token stream, sources/aliases/virtual tables/TYPE/casts/wildcards/direct CASE bindings и ограниченные VALUE/ЗНАЧЕНИЕ. Это конечный профиль, не общий SDBL/BSL parser |
| C09 | `projection`, `source_edits`: source-derived logical expectation, сохраненные узлы и byte edits. Неизмененные bytes сохраняются, ожидаемый digest считается отдельным потоковым проходом |
| C10 | `package_compiler`, `package`: frozen file inventory, Configuration/Language/_Перенос и исполнитель Copy/Project/Generate. Source/child/GeneratedType UUID сохраняются; отсутствующий объявленный Language блокируется |
| C11 | Frozen MergePlan и независимый settings guard. Дочернее правило сопоставляется с отдельной source-derived declaration identity, включая owner/tag/name; порядок правил заморожен |
| C12 | `identity::CompatibilityIdentityV1`: прежний seed и UUID служебной подсистемы, без перехода на новый алгоритм identity |
| C13 | Ограниченный lookup fold ASCII/русский/Ё отделен от библиотечной полной Unicode collision check. Имена вне lookup-алфавита не получают новых эквивалентностей |
| C14 | `predefined`: CodeType/CodeLength, numeric/string/decimal/empty/zero/name distinctions в исходном XML scope |
| C15 | Source-scoped contextual picture references, full resource policy и строгий readback. Обычные строки, похожие на QName, сравниваются как строки; QName normalization применяется только к известным слотам |
| C16 | `facts_cache`, `index`, `indexing`: отдельный owned V2 cache, scoped signatures для всех reuse gates, read-only V1 adapter и атомарная явная публикация |

Старые `metadata.rs`, `forms.rs`, `query.rs` и implementation части `stubs.rs` доступны только под `cfg(test)` как offline compatibility controls. `StubSpec` остается рабочим контрактом; `StubOutput` сохранен как compatibility DTO. Ни ошибка нового reader, ни отсутствие подходящего cache не переключают runtime на прежний producer.

## Namespace и конечный профиль

Смысл XML имени определяется URI и local name с учетом scope/default/rebinding. Element/attribute prefixes должны быть объявлены; известные QName-bearing slots (`xsi:type`, core Type, contextual Picture ref) не подменяют ошибку пустыми facts. Синтаксис QName использует NCName из [XML Namespaces](https://www.w3.org/TR/xml-names/#NT-QName) и [XML 1.0 Fifth Edition](https://www.w3.org/TR/xml/#NT-NameStartChar).

Исторический unqualified Form profile допускает буквальный `cfg:` metadata hint без объявления cfg в Type. Такой hint не получает выдуманный URI и не объявляется structured QName; limitation явно сохраняется. Правило не отменяет явное namespace binding, не распространяется на современную namespace-aware форму, произвольный prefix, foreign Type element или поврежденное QName.

Known weak references сохраняют required policy, когда цель подтверждена каталогом. Неизвестные lexical упоминания и неизвестные builtin picture names не становятся mandatory автоматически. Известный required endpoint с отсутствующим descriptor блокирует preview. Static QueryText передает typed mandatory endpoints из явно распознанных FROM/JOIN (включая comma-separated FROM lists), TYPE/cast/REFS и VALUE конструкций вместе с XML origin. Содержимое строк, параметры, комментарии и произвольные динамические выражения не выдаются за полностью распознанный обязательный граф. Для BSL поддерживается отдельный ограниченный literal reader; комментарии остаются optional причинами, required-wins сохраняется.

Профиль query не обещает полную синтаксическую/семантическую проверку языка, разрешение произвольных выражений, временных таблиц и динамически собранного текста. Для CommonForm/CommonTemplate не вводится новая глобальная full policy. Unknown constructs не являются доказательством отсутствия зависимостей.

## Cache и актуальность

Физический payload schema: `FACT_SCHEMA=2`; owned файл имеет суффикс `.source-facts-v2.sqlite3`. Семантики: Metadata `source-v2.3`, Registrars `source-v2.2`, References `source-v2.5`, Forms/Query `source-v2.4`, остальные текущие facets `source-v2.1`; consuming rule profile — `cf-rules-3`. Изменение extraction инвалидирует затронутые scoped facts даже при прежних XML/SHA/OID/manifest. Только изменение rules не заставляет перечитывать пригодные raw facts.

V1 остается read-only источником доказанно совместимых сведений и не переименовывается в V2. Обновление явное; preview/readiness не мигрируют cache. Перед commit публикации отмена сохраняет прежний результат, после успешного commit поздняя отмена не превращает успех в ложный Cancelled. Forms/query paths сейчас job-scoped и читаются из retained source files; наличие версии facet не означает, что все form/query payload уже persist в descriptor cache.

Proof включает не только выдаваемые файлы: в него входят невыдаваемые модули, их отсутствие/непустота, Configuration, FunctionalOption Content и необходимые listings. Изменение этих данных после preview блокирует старый план; serializer не пересчитывает policy по live source.

## Доказательства и происхождение

`profile_contract_tests` задает независимо построенные минимальные source/model cases для всех активных таблиц, включая отсутствие цели/кандидата, namespace collision, scalar protections, модули, source order, register suffixes и minimum choices. Неиспользуемые `AS_WORDS` и `REF_WORDS` — compatibility data без runtime callers; проверяются действующие inline policies. Старые JSON snapshots сохраняются как parity controls и не создаются из нового writer.

Новые readers/compiler/guards являются собственной прикладной реализацией. Datafrog, XML tokenizer, SQLite, HTTP/Git и другие библиотеки остаются внешними. Конечные правила и нормативные имена имеют преемственность с историческим прототипом; техническая замена не стирает эту историю и не является юридическим clean-room заключением. См. [provenance](provenance.md) и [LICENSING](../../LICENSING.md).

Catalogue Configuration использует тот же namespace-scoped source view и отклоняет foreign declarations до построения membership. Современная Form не принимает unqualified inner Type как CORE; compatibility empty namespace ограничена legacy Form. Full readback проверяет namespace validity actual XML до signature: повтор expanded attributes и reserved binding отклоняются.

Inventory доказывает presence только при точном совпадении пути. При несовпадении spelling используется authoritative provider.exists: Windows case aliases сохраняются, Git blob identity остается точной. Bytes существующих omitted modules хешируются; before/after listing и source hashes проверяются до и после выдачи.

Локальный артефакт0.8.6 проверен отдельно: [приёмка и ограничения](../reports/cf-wizard-0.8.6-acceptance.md).
