# Смысловое ревью GitHub/Gitea

Независимый read-only исполнитель: `/root/cf_github_semantic`; модель унаследована, фактический model ID не предоставлен (actual_model=unknown). Базовая ревизия78dede8a98a55e1bcd7fe4bd3a5530cdac3c2b0f; финальные29source hashes совпали с review-input.json. SHA256 манифеста:e6595abc96f7079a7e97f0f9926faece4249835936fd5172580513dc10542073.

Статус: **проверено**, открытых блокирующих замечаний по проверенному коду нет. Отчёт отражает статический проход и read-only воспроизведения; production/private приёмку не заменяет.

| Контракт | Цепочка и результат |
|---|---|
| R1 | qt_git_dialog → qt_view._connect_git → git_api.create_client: явный API selector и отдельные стратегии. Подпись активного источника берётся из открытого клиента. |
| R2 | paths.load, credential_target, GitFiles.__init__: старые настройки означают Gitea, прежний target/cache сохранены; GitHub namespace отдельный; local стратегия сохранена. |
| R3 | Смена API/адреса очищает secret/remember/ветки; удаление получает текущие provider/host. Нормализация до WinCred/HTTP, fixed origin, header-only PAT, запретredirect, безопасные ошибки без тела ответа. |
| R4 | Ref → commit → tree SHA, неrecursive деревья; усечение отвергается. GraphQL alias/type/OID/size/text/hash проверены; адресный fallback проверяет hash. GitFiles сохраняет path/LFS/collision-защиту. SQLite draft → checks → replace. |
| R5 | find_git_task передаёт SHA/directory/cancel. Tree diff первого родителя полный без commit-files cap, rename/delete учитываются, отмена до следующего запроса/публикации. |
| R6 | Ошибки индексирования/чтения журналируются без PAT; неудачное подключение сохраняет каталог/выбор/preview. Синтетические тесты отделены от private pilot. |

## Находки и адресные повторы

1. github.tree терял mode; commit_paths пропускал chmod-only изменения. До исправления read-only repro возвращал[]; после Blob.mode и сохранения mode обоими парсерами возвращает A.xml. Blob.matches продолжает сравнивать только bytes/hash: кэш и SQLite facts не нарушены.
2. После алгоритмического замечания Gitea GET URL-порог вынесен в Gitea.files. Проверен полный результат длинных кириллических путей и ограничения запросов; GitHub получает нейтральные пачки20.
3. Дополнительный адресный проход финального github.py dd66dc277c5aeab3747301b81536fc8a0f60caac9fd2181ab680cff8149ad6b1: GitHub.tree проверяет Git components, Windows rules применяются при материализации config. outside:notes/CON вне config не блокируют открытие и не записываются. bad:notes.xml, CON, C:\escape.xml, dot/dotdot, traversal и пустое имя внутри выбранного дерева отклоняются. objects_for_paths сопоставляет строки с известными владельцами и не открывает файловые пути. Граница записи не ослаблена; Gitea не затронута.

## Границы

Ревьюер не изменял файлы, не читал токены и не вызывал реальные сервисы. Private minimal-PAT/SSO, реальные rate budgets и время на полной рабочей конфигурации остаются отдельным пилотом. BSL/XML1С, платформенный runtime и CF-сборка не менялись и здесь не принимались. До merge проверяются окончательные head/base и настроенные checks PR.
