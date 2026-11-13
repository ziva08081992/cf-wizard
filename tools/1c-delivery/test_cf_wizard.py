"""Tests for the manual technical CF wizard with real source-only staging."""

from datetime import datetime
from contextlib import contextmanager
import json
import os
from pathlib import Path
import shutil
import tempfile
import tkinter as tk
import unittest
from unittest.mock import patch

from cf_wizard.adapters import (
    DesignerDependencyIndexes, DesignerExport, TechnicalPackage, discover_build_platforms, discover_ibcmd,
    preferred_platform,
)
from cf_wizard.application import JournaledError, Wizard
from cf_wizard.dependency_index import _facts
from cf_wizard.paths import PathPreferences, PathSettingsFile
from cf_wizard.view import WizardWindow
import native
from test_native import FakeNative


MD = ('<MetaDataObject xmlns="http://v8.1c.ru/8.3/MDClasses" '
      'xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" '
      'xmlns:xr="http://v8.1c.ru/8.3/xcf/readable" '
      'xmlns:v8="http://v8.1c.ru/8.1/data/core" xmlns:cfg="http://v8.1c.ru/8.1/data/enterprise/current-config">')
DOCUMENT = "Document.Заказ"
CATALOG = "Catalog.Склады"
PLATFORM_BUILD = ("8.3.27.2214", 0x8664)


class WizardTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix="Мастер CF ")
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
        temporary = patch("cf_wizard.adapters.tempfile.gettempdir", return_value=str(self.root))
        temporary.start()
        self.addCleanup(temporary.stop)
        self.export = self.root / "Исходные файлы"
        self.output = self.root / "Результат"
        self.output.mkdir()
        self.write("Configuration.xml", '<Configuration uuid="00000000-0000-0000-0000-000000000001">'
                   '<Properties><Name>Пример</Name></Properties>'
                   '<ChildObjects><Document>Заказ</Document><Catalog>Склады</Catalog></ChildObjects>'
                   '</Configuration>')
        self.write("Documents/Заказ.xml", '<Document uuid="00000000-0000-0000-0000-000000000002">'
                   '<Properties><Name>Заказ</Name></Properties>'
                   '<ChildObjects><Attribute uuid="00000000-0000-0000-0000-000000000004">'
                   '<Properties><Name>Склад</Name><Type><v8:Type>cfg:CatalogRef.Склады</v8:Type></Type></Properties>'
                   '</Attribute></ChildObjects></Document>')
        self.write("Catalogs/Склады.xml", '<Catalog uuid="00000000-0000-0000-0000-000000000003">'
                   '<Properties><Name>Склады</Name></Properties><ChildObjects/></Catalog>')
        self.platform = self.root / "8.3.27.2214" / "bin" / "ibcmd.exe"
        self.platform.parent.mkdir(parents=True)
        self.platform.write_bytes(b"fake platform")
        (self.platform.parent / "core83.dll").touch()
        build_reader = patch("cf_wizard.adapters._windows_binary_build",
                             side_effect=lambda path: PLATFORM_BUILD if path.is_file() else None)
        self.binary_build = build_reader.start()
        self.addCleanup(build_reader.stop)
        reader = DesignerExport()
        self.logs = self.root / "Журналы"
        self.wizard = Wizard(reader, TechnicalPackage(reader, self.logs))
        self.source = self.wizard.open_export(str(self.export / "Configuration.xml"))

    def write(self, relative, body):
        path = self.export / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(MD + body + "</MetaDataObject>", encoding="utf-8")

    def preview(self):
        return self.wizard.preview(self.source, "erp-123", {DOCUMENT}, str(self.output))

    @contextmanager
    def emulate_native(self, *, fail=None, inspect_stage=None):
        build_cf = native.build_cf
        runners = []

        def build(stage, *args, **kwargs):
            if inspect_stage:
                inspect_stage(stage)
            runner = FakeNative(stage, fail=fail)
            runners.append(runner)
            return build_cf(stage, *args, **{**kwargs, "command_runner": runner})

        with patch("cf_wizard.adapters.native.build_cf", build):
            yield runners

    def indexed_wizard(self):
        return Wizard(self.wizard.reader, self.wizard.builder,
                      DesignerDependencyIndexes(self.wizard.reader, self.root / "Индексы", self.logs))

    def test_git_branch_uses_separate_index_and_stages_only_required_files(self):
        from hashlib import sha1
        from cf_wizard.gitea import Blob, ExportSourceFactory, Tree

        register = "AccumulationRegister.Движения"
        module = "CommonModule.МалыйМодуль"
        configuration = self.export / "Configuration.xml"
        configuration.write_bytes(configuration.read_bytes().replace(
            b"</ChildObjects>",
            "<AccumulationRegister>Движения</AccumulationRegister><CommonModule>МалыйМодуль</CommonModule></ChildObjects>".encode(), 1))
        self.write("AccumulationRegisters/Движения.xml",
                   '<AccumulationRegister uuid="00000000-0000-0000-0000-000000000006">'
                   '<Properties><Name>Движения</Name></Properties><ChildObjects/></AccumulationRegister>')
        self.write("CommonModules/МалыйМодуль.xml",
                   '<CommonModule uuid="00000000-0000-0000-0000-000000000007">'
                   '<Properties><Name>МалыйМодуль</Name></Properties></CommonModule>')
        module_file = self.export / "CommonModules/МалыйМодуль/Ext/Module.bsl"
        module_file.parent.mkdir(parents=True)
        module_file.write_text("Процедура Пустая()\nКонецПроцедуры\n", encoding="utf-8")
        unrelated = self.export / "Catalogs/Склады/Ext/Ненужный.bin"
        unrelated.parent.mkdir(parents=True)
        unrelated.write_bytes(b"unused layout")
        document = self.export / "Documents/Заказ.xml"
        document.write_bytes(document.read_bytes().replace(
            b"</Properties>", f'<RegisterRecords><xr:Item xsi:type="xr:MDObjectRef">{register}</xr:Item>'
            '</RegisterRecords></Properties>'.encode(), 1))

        bodies = {path.relative_to(self.export).as_posix(): path.read_bytes()
                  for path in self.export.rglob("*") if path.is_file()}

        def blob(data):
            return Blob(sha1(f"blob {len(data)}\0".encode() + data).hexdigest(), len(data))

        revision = "a" * 40

        class Client:
            server = "https://git.example.test"
            repository = "owner/repo"

            def __init__(self):
                self.requested = []
                self.listed = []
                self.trees = {}

            def branch_sha(self, branch):
                return revision

            def config_tree(self, sha, directory):
                assert sha == revision and directory == "config"
                return self.directory_entries("")

            def tree(self, oid):
                return self.directory_entries(self.trees[oid])

            def directory_entries(self, directory):
                self.listed.append(directory)
                entries = {}
                prefix = directory + "/" if directory else ""
                for path, data in bodies.items():
                    if not path.startswith(prefix):
                        continue
                    name, sep, _ = path[len(prefix):].partition("/")
                    if sep:
                        child = prefix + name
                        oid = sha1((revision + child).encode()).hexdigest()
                        self.trees[oid] = child
                        entries[name] = Tree(oid)
                    else:
                        entries[name] = blob(data)
                return entries

            def file(self, sha, path, entry):
                assert sha == revision and entry.matches(bodies[path[7:]])
                self.requested.append(path)
                return bodies[path[7:]]

            def files(self, sha, paths):
                assert sha == revision
                self.requested.extend(paths)
                return {name: bodies[name[7:]] for name in paths}

        wizard = self.indexed_wizard()
        factory = ExportSourceFactory(wizard.open_export, self.root / "Git")
        client = Client()
        git = factory.strategy("git")(client, "newmaster", "config")
        self.assertEqual(client.requested, ["config/Configuration.xml"])
        self.assertEqual(set(client.listed), {"", "Documents", "AccumulationRegisters", "Catalogs", "CommonModules"})
        self.assertNotEqual(wizard.index_path(git, ""), wizard.index_path(self.source, ""))
        with self.assertRaisesRegex(ValueError, "индекс выбранной ревизии"):
            wizard.preview(git, "", {register}, str(self.output))
        local = wizard.refresh_index(wizard.open_export(str(self.export)), "").source
        update = wizard.refresh_index(git, "")
        git = update.source
        git_log = update.log.read_text(encoding="utf-8")
        self.assertIn(f"ветка newmaster, SHA {revision}", git_log)
        self.assertIn("Подготовка XML-описаний из Gitea и кэша", git_log)
        self.assertNotIn("config/CommonModules/МалыйМодуль/Ext/Module.bsl", client.requested)

        local_preview = wizard.preview(local, "local", {register, module}, str(self.output))
        git_preview = wizard.preview(git, "git", {register, module}, str(self.output))
        self.addCleanup(wizard.discard, local_preview)
        self.addCleanup(wizard.discard, git_preview)
        self.assertEqual(local_preview.composition, git_preview.composition)
        self.assertIn(DOCUMENT, {stub.name for stub in git_preview.dependencies})
        with self.emulate_native():
            first = wizard.build(local_preview, str(self.platform))
            second = wizard.build(git_preview, str(self.platform))
        self.assertEqual(next(first.glob("*.merge-settings.xml")).read_bytes(),
                         next(second.glob("*.merge-settings.xml")).read_bytes())
        self.assertIn("config/CommonModules/МалыйМодуль/Ext/Module.bsl", client.requested)
        self.assertNotIn("config/Catalogs/Склады/Ext/Ненужный.bin", client.requested)

        old = wizard.index_path(local, "").read_bytes()
        seed_client = Client()
        release = factory.strategy("git")(seed_client, "release", "config", self.export, wizard.index_path(local, ""))
        update = wizard.refresh_index(release, "")
        self.assertEqual(update.added, 0)
        self.assertEqual(update.changed, 0)
        self.assertGreater(update.unchanged, 0)
        self.assertEqual(seed_client.requested, [])
        self.assertEqual(wizard.index_path(local, "").read_bytes(), old)
        self.assertNotEqual(wizard.index_path(git, ""), wizard.index_path(release, ""))

        revision = "c" * 40
        bodies["Catalogs/Склады.xml"] += b"\n<!-- changed in next revision -->"
        next_client = Client()
        newer = factory.strategy("git")(next_client, "newmaster", "config")
        self.assertEqual(wizard.index_path(git, ""), wizard.index_path(newer, ""))
        self.assertNotEqual(newer.identity, git.identity)
        self.assertEqual(next_client.requested, [])
        update = wizard.refresh_index(newer, "")
        self.assertEqual((update.added, update.changed, update.removed), (0, 1, 0))
        self.assertEqual(next_client.requested, ["config/Catalogs/Склады.xml"])

    def test_gitea_client_reads_pinned_tree_pages_and_hides_transport_details(self):
        from http.client import BadStatusLine
        from io import BytesIO
        from urllib.error import HTTPError
        from cf_wizard.gitea import Blob, GiteaClient, GiteaError, Tree

        token, revision, tree_oid, blob_oid = "test-token", "a" * 40, "b" * 40, "c" * 40
        client = GiteaClient("https://git.example.test", "owner/repo", token)
        with patch.object(client, "_json", return_value=[{
            "ref": "refs/heads/feature/pilot", "object": {"sha": revision},
        }]) as send:
            self.assertEqual(client.branch_sha("feature/pilot"), revision)
            self.assertEqual(send.call_args.args[0], "git/refs/heads/feature/pilot")

        def page(oid, number, count, truncated, *entries):
            return {"sha": oid, "page": number, "total_count": count,
                    "truncated": truncated, "tree": list(entries)}

        with patch.object(client, "_json", side_effect=[
            page(revision, 1, 1, False, {"path": "config", "type": "tree", "mode": "040000", "sha": tree_oid}),
            page(tree_oid, 1, 2, True, {"path": "Configuration.xml", "type": "blob",
                                        "mode": "100644", "sha": blob_oid, "size": 15}),
            page(tree_oid, 2, 2, False, {"path": "Documents", "type": "tree", "mode": "040000", "sha": tree_oid}),
        ]) as send:
            self.assertEqual(client.config_tree(revision, "config"), {
                "Configuration.xml": Blob(blob_oid, 15), "Documents": Tree(tree_oid),
            })
            self.assertEqual(client.config_tree(revision, "config")["Documents"], Tree(tree_oid))
            self.assertEqual([call.args[1]["page"] for call in send.call_args_list], [1, 1, 2])
            self.assertTrue(all("recursive" not in call.args[1] for call in send.call_args_list))
        with patch.object(client, "_json", return_value=page("d" * 40, 1, 1, False)):
            with self.assertRaisesRegex(GiteaError, "неполный каталог"):
                client.tree("d" * 40)

        for status in (302, 401, 403, 404, None):
            error = BadStatusLine(token) if status is None else HTTPError(
                "https://git.example.test", status, token, {}, BytesIO())
            with patch.object(client._opener, "open", side_effect=error) as send:
                with self.assertRaises(GiteaError) as caught:
                    client.branches()
                self.assertEqual(caught.exception.status, status)
                self.assertNotIn(token, str(caught.exception))
                request = send.call_args.args[0]
                self.assertEqual(request.get_header("Authorization"), f"token {token}")
                self.assertNotIn(token, request.full_url)

    def test_extension_is_rejected_for_local_and_gitea(self):
        from hashlib import sha1
        from cf_wizard.gitea import Blob, ExportSourceFactory

        path = self.export / "Configuration.xml"
        data = path.read_bytes().replace(b"</Properties>",
                                        b"<ConfigurationExtensionPurpose>Customization</ConfigurationExtensionPurpose></Properties>",
                                        1)
        path.write_bytes(data)
        with self.assertRaisesRegex(ValueError, "выгрузка расширения"):
            self.wizard.open_export(str(path))

        class Client:
            server = "https://git.example.test"
            repository = "owner/repo"

            def branch_sha(self, branch):
                return "b" * 40

            def config_tree(self, sha, directory):
                oid = sha1(f"blob {len(data)}\0".encode() + data).hexdigest()
                return {"Configuration.xml": Blob(oid, len(data))}

            def file(self, sha, name, entry):
                return data

        factory = ExportSourceFactory(self.wizard.open_export, self.root / "Git")
        with self.assertRaisesRegex(ValueError, "выгрузка расширения"):
            factory.strategy("git")(Client(), "newmaster", "config")

    def test_dependency_index_updates_only_changed_objects_and_rolls_back_on_error(self):
        wizard = self.indexed_wizard()
        logs = self.root / "Журналы индекса"
        first = wizard.refresh_index(self.source, "", str(logs))
        self.assertEqual((first.added, first.changed, first.removed), (2, 0, 0))
        self.assertEqual(first.log.parent, logs)
        journal = first.log.read_text(encoding="utf-8")
        self.assertIn(f"Создание индекса {first.path}", journal)
        self.assertIn(str(self.export), journal)
        self.assertIn("Добавлено 2, изменено 0, удалено 0, переиспользовано 0.", journal)
        document = self.export / "Documents/Заказ.xml"
        original = document.stat()
        document.write_bytes(document.read_bytes().replace("<Name>Склад</Name>".encode(), "<Name>Отдел</Name>".encode()))
        os.utime(document, ns=(original.st_atime_ns, original.st_mtime_ns))
        self.assertEqual((document.stat().st_size, document.stat().st_mtime_ns),
                         (original.st_size, original.st_mtime_ns))
        with patch("cf_wizard.dependency_index._facts", side_effect=ValueError("Сбой разбора")):
            with self.assertRaisesRegex(JournaledError, "Сбой разбора") as caught:
                wizard.refresh_index(self.source, "", str(logs))
        error_log = caught.exception.log
        self.assertEqual(error_log.parent, logs)
        self.assertNotEqual(error_log, first.log)
        self.assertIn("Сбой разбора", error_log.read_text(encoding="utf-8"))
        with patch("cf_wizard.dependency_index._facts", wraps=_facts) as parse:
            updated = wizard.refresh_index(self.source, "")
            self.assertEqual((updated.path, updated.changed, updated.unchanged), (first.path, 1, 1))
            journal = updated.log.read_text(encoding="utf-8")
            self.assertIn(f"Обновление индекса {first.path}", journal)
            self.assertIn("Добавлено 0, изменено 1, удалено 0, переиспользовано 1.", journal)
            self.assertEqual(parse.call_count, 1)
            unchanged = wizard.refresh_index(self.source, "")
            self.assertEqual((unchanged.added, unchanged.changed, unchanged.removed, unchanged.unchanged), (0, 0, 0, 2))
            self.assertEqual(parse.call_count, 1)

        configuration = self.export / "Configuration.xml"
        configuration.write_bytes(configuration.read_bytes().replace("<Catalog>Склады</Catalog>".encode(),
                                                                     "<Catalog>Отделы</Catalog>".encode()))
        catalog = self.export / "Catalogs/Склады.xml"
        catalog.with_name("Отделы.xml").write_bytes(catalog.read_bytes().replace("Склады".encode(), "Отделы".encode()))
        catalog.unlink()
        updated = wizard.refresh_index(self.source, "")
        self.assertEqual((updated.added, updated.changed, updated.removed, updated.unchanged), (1, 0, 1, 1))
        self.assertIn("Catalog.Отделы", updated.source.objects)

    def test_dependency_index_separates_exports_and_persists_directory_per_export(self):
        wizard = self.indexed_wizard()
        alternate = self.root / "Вторая выгрузка"
        shutil.copytree(self.export, alternate)
        other = wizard.open_export(str(alternate / "Configuration.xml"))
        shared = self.root / "Общая папка SQLite"
        first = wizard.refresh_index(self.source, str(shared))
        second = wizard.refresh_index(other, str(shared))
        self.assertEqual(first.path.parent, second.path.parent)
        self.assertNotEqual(first.path, second.path)
        self.assertEqual(wizard.index_path(wizard.open_export(str(self.export)), str(shared)), first.path)
        with self.assertRaisesRegex(ValueError, "вне XML-выгрузки"):
            wizard.refresh_index(self.source, str(self.export / "Индексы"))
        settings = PathSettingsFile(self.root / "paths.json")
        preferences = PathPreferences({"index_directory": str(shared)}, {"index_directory": str(shared)},
                                      {first.source.identity: str(shared), other.identity: str(self.root / "Другие индексы")})
        settings.save(preferences)
        self.assertEqual(settings.load(), preferences)

    def test_registrar_index_preserves_stubs_and_checks_register_records(self):
        register = "AccumulationRegister.Движения"
        config = self.export / "Configuration.xml"
        config.write_bytes(config.read_bytes().replace(
            b"</ChildObjects>", "<AccumulationRegister>Движения</AccumulationRegister></ChildObjects>".encode(), 1))
        self.write("AccumulationRegisters/Движения.xml",
                   '<AccumulationRegister uuid="00000000-0000-0000-0000-000000000006">'
                   '<Properties><Name>Движения</Name></Properties><ChildObjects/></AccumulationRegister>')
        document = self.export / "Documents/Заказ.xml"
        document.write_bytes(document.read_bytes().replace(b"</Properties>",
            f'<RegisterRecords><xr:Item xsi:type="xr:MDObjectRef">{register}</xr:Item></RegisterRecords></Properties>'.encode(),
            1))
        wizard = self.indexed_wizard()
        source = wizard.refresh_index(self.source, "").source
        baseline = self.wizard.preview(source, "", {register}, str(self.output))
        self.addCleanup(self.wizard.discard, baseline)
        indexed = wizard.preview(source, "", {register}, str(self.output))
        self.addCleanup(wizard.discard, indexed)
        self.assertEqual(indexed.composition, baseline.composition)
        self.assertEqual({stub.name for stub in indexed.dependencies}, {DOCUMENT})
        self.assertIn("запросов регистраторов 1", indexed.log.read_text(encoding="utf-8"))
        with self.emulate_native():
            baseline_package = self.wizard.build(baseline, str(self.platform))
            indexed_package = wizard.build(indexed, str(self.platform))
        self.assertEqual(next(indexed_package.glob("*.merge-settings.xml")).read_bytes(),
                         next(baseline_package.glob("*.merge-settings.xml")).read_bytes())

        original = document.stat()
        document.write_bytes(document.read_bytes().replace(register.encode(), "AccumulationRegister.ОстаткиА".encode()))
        os.utime(document, ns=(original.st_atime_ns, original.st_mtime_ns))
        self.assertEqual((document.stat().st_size, document.stat().st_mtime_ns),
                         (original.st_size, original.st_mtime_ns))
        updated = wizard.preview(source, "", {register}, str(self.output))
        self.addCleanup(wizard.discard, updated)
        self.assertEqual(updated.dependencies, ())
        self.assertIn("переходов на XML 1", updated.log.read_text(encoding="utf-8"))

    def add_module(self, text):
        module = "ОбщийТестовыйМодуль"
        configuration = self.export / "Configuration.xml"
        data = configuration.read_text(encoding="utf-8")
        configuration.write_text(data.replace("<ChildObjects>", f"<ChildObjects><CommonModule>{module}</CommonModule>", 1),
                                 encoding="utf-8")
        self.write(f"CommonModules/{module}.xml",
                   '<CommonModule uuid="00000000-0000-0000-0000-000000000005">'
                   f'<Properties><Name>{module}</Name></Properties><ChildObjects/></CommonModule>')
        bsl = self.export / f"CommonModules/{module}/Ext/Module.bsl"
        bsl.parent.mkdir(parents=True)
        bsl.write_text(text, encoding="utf-8")
        self.source = self.wizard.open_export(str(self.export))
        return "CommonModule." + module, bsl

    def test_comment_only_stub_can_be_omitted_with_merge_settings(self):
        self.write("Catalogs/Склады.xml", '<Catalog uuid="00000000-0000-0000-0000-000000000003">'
                   '<InternalInfo><xr:GeneratedType name="CatalogRef.Склады" category="Ref"/></InternalInfo>'
                   '<Properties><Name>Склады</Name></Properties><ChildObjects/></Catalog>')
        module, bsl = self.add_module("// ЗНАЧЕНИЕ(Справочник.Склады.Основной)\n")
        preview = self.wizard.preview(self.source, "", {module}, str(self.output))
        stub, = preview.dependencies
        self.assertEqual(stub.name, CATALOG)
        self.assertEqual(tuple(cause.owner for cause in stub.causes), (module,))
        self.assertFalse(stub.required)
        with self.emulate_native() as runners:
            package = self.wizard.build(preview, str(self.platform), {CATALOG})
        runner, = runners
        self.assertEqual(len(runner.calls), 3)
        settings, = package.glob("*.merge-settings.xml")
        self.assertNotIn(CATALOG, settings.read_text(encoding="utf-8-sig"))
        self.assertEqual(bsl.read_text(encoding="utf-8"), "// ЗНАЧЕНИЕ(Справочник.Склады.Основной)\n")
        self.assertIn(f"Исключены заглушки.\n{CATALOG}", preview.log.read_text(encoding="utf-8"))
        self.assertFalse(runner.stage.parent.exists())

    def test_stub_with_an_xml_cause_is_locked_across_owner_types(self):
        module, _ = self.add_module("// ЗНАЧЕНИЕ(Справочник.Склады.Основной)\n")
        preview = self.wizard.preview(self.source, "", {module, DOCUMENT}, str(self.output))
        stub, = preview.dependencies
        self.assertTrue(stub.required)
        self.assertEqual({cause.owner: cause.required for cause in stub.causes}, {module: False, DOCUMENT: True})
        with patch("cf_wizard.adapters.native.build_cf") as build:
            with self.assertRaisesRegex(ValueError, "обязательные ссылки"):
                self.wizard.build(preview, str(self.platform), {CATALOG})
        build.assert_not_called()
        self.assertIn("Предпросмотр закрыт.", preview.log.read_text(encoding="utf-8"))

    def test_exclusion_requires_same_source_snapshot(self):
        module, bsl = self.add_module("// ЗНАЧЕНИЕ(Справочник.Склады.Основной)\n")
        preview = self.wizard.preview(self.source, "", {module}, str(self.output))
        bsl.write_text("// Новое содержимое\n", encoding="utf-8")
        with patch("cf_wizard.adapters.native.build_cf") as build:
            with self.assertRaisesRegex(ValueError, "выгрузка изменилась"):
                self.wizard.build(preview, str(self.platform), {CATALOG})
        build.assert_not_called()
        self.assertEqual(list(self.output.iterdir()), [])

    def test_file_and_folder_resolve_to_the_same_catalog(self):
        self.assertEqual(self.wizard.open_export(str(self.export)), self.source)
        self.assertEqual(self.wizard.open_export(f'"{self.export / "Configuration.xml"}"'), self.source)
        self.assertEqual(self.source.objects, (CATALOG, DOCUMENT))
        with self.assertRaisesRegex(ValueError, "Configuration.xml"):
            self.wizard.open_export(str(self.export / "Documents/Заказ.xml"))

    def test_real_staging_reports_stub_and_mocked_readback_publishes_complete_package(self):
        with patch("cf_wizard.adapters.tempfile.TemporaryDirectory", side_effect=AssertionError("Предпросмотр пишет XML")):
            preview = self.preview()
        self.assertEqual(preview.task, "ERP-123")
        self.assertEqual(preview.selected, (DOCUMENT,))
        self.assertEqual([(s.name, s.referred_by) for s in preview.dependencies], [(CATALOG, DOCUMENT)])
        self.assertEqual(list(self.output.iterdir()), [])
        expected_settings = []

        def check_stage(stage):
            self.assertEqual((stage / "Documents/Заказ.xml").read_bytes(),
                             (self.export / "Documents/Заказ.xml").read_bytes())
            expected_settings.append((stage.parent / "merge-settings.xml").read_bytes())

        with (self.emulate_native(inspect_stage=check_stage) as runners,
              patch("cf_wizard.adapters.resolve_dependencies", side_effect=AssertionError("Повторный расчёт"))):
            package = self.wizard.build(preview, str(self.platform))
        runner, = runners
        self.assertEqual([call[0][2] for call in runner.calls], ["create", "import", "export"])
        self.assertEqual(package, self.output / runner.stage.parent.name)
        self.assertTrue(package.name.startswith("ERP-123-"))
        self.assertEqual(set(self.output.iterdir()), {package})
        files = list(package.iterdir())
        self.assertEqual(len(files), 2)
        self.assertEqual(len(list(package.glob("*.cf"))), 1)
        settings, = package.glob("*.merge-settings.xml")
        self.assertEqual(settings.read_bytes(), expected_settings[0])
        self.assertFalse(runner.stage.parent.exists())
        journal = preview.log.read_text(encoding="utf-8")
        for message in ("Предпросмотр готов", "Использую рассчитанный план", "03-export.log",
                        "export completed", "Временные файлы очищены"):
            self.assertIn(message, journal)
        self.assertGreater(len(journal.splitlines()), 5)

    def test_blank_task_uses_local_date_and_time_and_keeps_existing_packages(self):
        now = datetime(2026, 11, 8, 14, 25, 10)
        base = "2026-11-08_14-25-10"
        self.assertFalse(list(self.output.iterdir()))
        for name in (base, base + "-2"):
            preview = self.wizard.preview(self.source, "  ", {DOCUMENT}, str(self.output))
            self.assertIsNone(preview.task)
            self.assertEqual(preview.destination, self.output)
            with (patch("cf_wizard.adapters.datetime") as clock,
                  self.emulate_native()):
                clock.now.return_value = now
                package = self.wizard.build(preview, str(self.platform))
            self.assertEqual(package, self.output / name)
        self.assertEqual({path.name for path in self.output.iterdir()}, {base, base + "-2"})

    def test_task_label_accepts_free_text_and_produces_safe_package_paths(self):
        examples = (
            ("1235", "1235-"),
            ("08/11/2026", "08_11_2026-"),
            ("Релиз: склад/приёмка", "Релиз_ склад_приёмка-"),
            ("NUL.txt", "_NUL.txt-"),
            ("А" * 100, "А" * 64 + "-"),
        )
        for task, prefix in examples:
            with self.subTest(task=task):
                preview = self.wizard.preview(self.source, task, {DOCUMENT}, "")
                self.addCleanup(self.wizard.discard, preview)
                self.assertEqual(preview.task, task)
                self.assertIsNone(preview.destination)
                with self.emulate_native():
                    package = self.wizard.build(preview, str(self.platform), destination=str(self.output))
                self.assertEqual(package.parent, self.output)
                self.assertTrue(package.name.startswith(prefix))
                self.assertEqual(len(list(package.glob("*.cf"))), 1)

    def test_discovery_prefers_project_platform_and_offers_other_installations(self):
        program_files = self.root / "Program Files"
        pinned = program_files / "1cv8/8.3.27.2214/bin/ibcmd.exe"
        other = program_files / "1cv8/8.5.4.1878/bin/ibcmd.exe"
        for binary in (pinned, other):
            binary.parent.mkdir(parents=True)
            binary.write_bytes(b"test")
        with patch.dict("os.environ", {"ProgramW6432": str(program_files), "ProgramFiles": str(program_files)},
                        clear=True):
            self.assertEqual(discover_ibcmd(), ((pinned, other), pinned))
            pinned.unlink()
            self.assertEqual(discover_ibcmd(), ((other,), None))

    def test_auto_uses_project_designer_and_keeps_manual_ibcmd(self):
        designer = self.platform.with_name("1cv8.exe")
        designer.touch()
        other = self.root / "8.5.4.1878/bin/1cv8.exe"
        self.assertIsNone(preferred_platform((other,), "auto"))
        with patch("cf_wizard.adapters._platform_roots", return_value=(self.root,)):
            installed = discover_build_platforms()
            self.assertEqual(preferred_platform(installed, "auto"), designer)
            self.assertEqual(preferred_platform(installed, "designer"), designer)
            self.assertEqual(preferred_platform(installed, "ibcmd"), self.platform)
            designer.unlink()
            self.assertIsNone(preferred_platform(discover_build_platforms(), "auto"))

    def test_mode_persistence_keeps_separate_platform_paths_and_reads_old_settings(self):
        settings = PathSettingsFile(self.root / "paths.json")
        paths = {"platform": str(self.platform), "designer": str(self.platform.with_name("1cv8.exe"))}
        preferences = PathPreferences(paths, build_mode="designer")
        settings.save(preferences)
        self.assertEqual(settings.load(), preferences)
        data = json.loads(settings.path.read_text(encoding="utf-8"))
        del data["build_mode"]
        settings.path.write_text(json.dumps(data), encoding="utf-8")
        self.assertEqual(settings.load(), PathPreferences(paths))

    def test_ibcmd_must_be_in_installed_platform_bin(self):
        executable = self.root / "Мои утилиты" / "ibcmd.exe"
        executable.parent.mkdir()
        executable.write_bytes(b"fake platform")
        preview = self.preview()
        with self.assertRaisesRegex(ValueError, "из папки bin"):
            self.wizard.build(preview, str(executable))
        self.assertEqual(list(self.output.iterdir()), [])

    def test_ibcmd_and_adjacent_core_must_match(self):
        core = self.platform.parent / "core83.dll"
        for wrong_build in (("8.3.27.2200", PLATFORM_BUILD[1]), (PLATFORM_BUILD[0], 0x014C)):
            with self.subTest(build=wrong_build):
                preview = self.preview()
                self.binary_build.side_effect = lambda path: wrong_build if path == core else PLATFORM_BUILD
                with self.assertRaisesRegex(ValueError, "8.3.27.2214"):
                    self.wizard.build(preview, str(self.platform))
                self.assertEqual(list(self.output.iterdir()), [])

    def test_logs_folder_cannot_modify_source_export(self):
        logs = self.export / "Журналы"
        with self.assertRaisesRegex(ValueError, "папку журналов вне XML-выгрузки"):
            self.wizard.preview(self.source, "", {DOCUMENT}, str(self.output), str(logs))
        self.assertFalse(logs.exists())

    def test_partial_export_fails_when_dependency_descriptor_is_missing(self):
        (self.export / "Catalogs/Склады.xml").unlink()
        before = set(self.root.iterdir())
        with self.assertRaisesRegex(ValueError, "отсутствуют описания объектов") as failed:
            self.preview()
        self.assertEqual(set(self.root.iterdir()) - before, {self.logs})
        log, = self.logs.glob("*.log")
        self.assertIn(str(log), str(failed.exception))
        self.assertIn("Ошибка предпросмотра", log.read_text(encoding="utf-8"))

    def test_changed_settings_cannot_be_used_for_build(self):
        preview = self.preview()
        prepare = self.wizard.builder._prepare

        def tamper(current, run, excluded, plan):
            proof = prepare(current, run, excluded, plan)
            (run / "merge-settings.xml").write_text("<Settings/>", encoding="utf-8")
            return proof

        with (patch.object(self.wizard.builder, "_prepare", side_effect=tamper),
              patch("cf_wizard.adapters.native.build_cf") as build):
            with self.assertRaisesRegex(ValueError, "изменились"):
                self.wizard.build(preview, str(self.platform))
        build.assert_not_called()

    def test_preview_plan_is_rejected_if_files_of_a_stub_change(self):
        preview = self.preview()
        new_file = self.export / "Catalogs/Склады/Ext/Predefined.xml"
        new_file.parent.mkdir(parents=True)
        new_file.write_text("<PredefinedData/>", encoding="utf-8")
        with patch("cf_wizard.adapters.native.build_cf") as build:
            with self.assertRaisesRegex(ValueError, "Исходники зависимостей изменились"):
                self.wizard.build(preview, str(self.platform))
        build.assert_not_called()
        self.assertEqual(list(self.output.iterdir()), [])

    def test_native_failure_keeps_logs_without_publishing(self):
        preview = self.preview()
        with self.emulate_native(fail="export") as runners:
            with self.assertRaisesRegex(RuntimeError, "03-export") as failed:
                self.wizard.build(preview, str(self.platform))
        self.assertEqual(list(self.output.iterdir()), [])
        runner, = runners
        self.assertFalse(runner.stage.parent.exists())
        self.assertNotIn(str(runner.stage.parent), str(failed.exception))
        journal = preview.log.read_text(encoding="utf-8")
        self.assertIn("Ошибка сборки", journal)
        self.assertIn("native failure 102", journal)

    def test_view_keeps_marks_when_filtering_and_invalidates_preview_on_selection_change(self):
        try:
            root = tk.Tk()
        except tk.TclError as exc:
            self.skipTest(str(exc))
        root.withdraw()
        self.addCleanup(root.destroy)
        window = WizardWindow(root, self.wizard, (self.platform,), self.platform)
        self.assertEqual(window.platform.get(), str(self.platform))
        window.catalog = self.source
        window.render_objects()
        self.assertEqual(window.objects.item("Document", "text"), "Документы (1)")
        window.toggle("Document")
        self.assertFalse(window.selected)
        window.objects.focus("Document")
        window.toggle_focused(None)
        self.assertEqual(window.objects.parent(DOCUMENT), "Document")
        self.assertEqual(window.objects.item(DOCUMENT, "text"), "Заказ")
        window.toggle(DOCUMENT)
        self.assertEqual(window.whole.item(window.whole.get_children()[0], "values"), ("Документ.Заказ",))
        window.busy = True
        window.open_export()
        self.assertEqual(window.selected, {DOCUMENT})
        self.assertEqual(window.catalog, self.source)
        window.busy = False
        window.preview = self.preview()
        window.destination.set(str(self.output))
        window.platform.set(str(self.platform))
        self.assertFalse(window.build_button.instate(["disabled"]))
        window.search.set("справочники")
        self.assertEqual(window.objects.get_children(), ("Catalog",))
        self.assertEqual(window.objects.parent(CATALOG), "Catalog")
        self.assertEqual(window.selected, {DOCUMENT})
        window.toggle(CATALOG)
        self.assertEqual(window.selected, {DOCUMENT, CATALOG})
        self.assertIsNone(window.preview)
        self.assertTrue(window.build_button.instate(["disabled"]))

    def test_paths_persist_and_each_dialog_uses_its_own_location(self):
        settings = PathSettingsFile(self.root / "Профиль" / "paths.json")
        export_file = self.export / "Configuration.xml"
        settings.save(PathPreferences({
            "export": export_file.as_posix(),
            "destination": self.output.as_posix(),
            "platform": self.platform.as_posix(),
            "logs": self.logs.as_posix(),
        }))
        try:
            root = tk.Tk()
        except tk.TclError as exc:
            self.skipTest(str(exc))
        root.withdraw()
        self.addCleanup(root.destroy)
        window = WizardWindow(root, self.wizard, paths=settings.load(), save_paths=settings.save)
        self.assertEqual(window.export.get(), str(export_file))
        self.assertEqual(window.destination.get(), str(self.output))
        self.assertEqual(window.platform.get(), str(self.platform))
        self.assertEqual(window.logs.get(), str(self.logs))

        with patch("cf_wizard.view.filedialog.askopenfilename", return_value="") as choose_file:
            window.browse_configuration()
            self.assertEqual(choose_file.call_args.kwargs["initialdir"], str(self.export))
            self.assertEqual(choose_file.call_args.kwargs["initialfile"], "Configuration.xml")
            window.browse_platform()
            self.assertEqual(choose_file.call_args.kwargs["initialdir"], str(self.platform.parent))
            self.assertEqual(choose_file.call_args.kwargs["initialfile"], "ibcmd.exe")

        chosen = self.root / "Новая папка"
        chosen.mkdir()
        with patch("cf_wizard.view.filedialog.askdirectory", return_value=chosen.as_posix()) as choose_folder:
            window.browse_folder("destination")
            self.assertEqual(choose_folder.call_args.kwargs["initialdir"], str(self.output))
            self.assertEqual(window.destination.get(), str(chosen))
            window.destination.set("")
            choose_folder.return_value = ""
            window.browse_folder("destination")
            self.assertEqual(choose_folder.call_args.kwargs["initialdir"], str(chosen))
            window.browse_folder("export")
            self.assertEqual(choose_folder.call_args.kwargs["initialdir"], str(self.export))

        custom_logs = self.root / "Выбранные журналы"
        custom_logs.mkdir()
        with patch("cf_wizard.view.filedialog.askdirectory", return_value=custom_logs.as_posix()):
            window.browse_folder("logs")
        preview = self.wizard.preview(self.source, "", {DOCUMENT}, str(self.output), window.logs.get())
        self.assertEqual(preview.log.parent, custom_logs)
        window.preview, window.last_log = preview, preview.log
        window.update_buttons()
        with patch("cf_wizard.view.os.startfile") as startfile:
            window.open_logs_folder()
            startfile.assert_called_with(str(custom_logs))
            window.log_button.invoke()
            startfile.assert_called_with(str(preview.log))
        with patch.object(root, "destroy") as destroy:
            window.close()
        destroy.assert_called_once()
        self.assertIn("Предпросмотр закрыт.", preview.log.read_text(encoding="utf-8"))
        self.assertTrue(preview.log.is_file())
        reopened = settings.load()
        self.assertEqual(reopened.values["logs"], str(custom_logs))
        self.assertEqual(reopened.values["destination"], "")
        self.assertEqual(reopened.values["export"], str(export_file))
        self.assertEqual(reopened.last_dirs["destination"], str(chosen))

    def test_invalid_path_settings_fall_back_to_defaults(self):
        settings = PathSettingsFile(self.root / "paths.json")
        settings.path.write_text("{invalid", encoding="utf-8")
        self.assertEqual(settings.load(), PathPreferences())

    def test_mouse_changes_marks_only_at_checkbox(self):
        try:
            root = tk.Tk()
        except tk.TclError as exc:
            self.skipTest(str(exc))
        self.addCleanup(root.destroy)
        root.attributes("-alpha", 0.0)
        window = WizardWindow(root, self.wizard)
        window.catalog = self.source
        window.search.set("заказ")
        window.preview = preview = self.preview()
        root.geometry("850x610")
        root.update()
        for button in (window.preview_button, window.build_button, window.open_button):
            self.assertTrue(button.winfo_ismapped())

        tree = window.objects
        x, y, width, height = tree.bbox(DOCUMENT, "#0")
        mark_x, _, mark_width, _ = tree.bbox(DOCUMENT, "mark")
        row_y = y + height // 2
        tree.event_generate("<Button-1>", x=x + width // 2, y=row_y)
        self.assertEqual(tree.selection(), (DOCUMENT,))
        tree.event_generate("<Button-1>", x=mark_x + 3, y=row_y)
        self.assertFalse(window.selected)
        self.assertIs(window.preview, preview)

        checkbox = mark_x + mark_width // 2
        tree.event_generate("<Button-1>", x=checkbox, y=row_y)
        self.assertEqual(window.selected, {DOCUMENT})
        self.assertIsNone(window.preview)
        self.assertIn("Предпросмотр закрыт.", preview.log.read_text(encoding="utf-8"))
        self.assertTrue(preview.log.is_file())
        tree.event_generate("<Button-1>", x=checkbox, y=row_y)
        self.assertFalse(window.selected)


if __name__ == "__main__":
    unittest.main()
