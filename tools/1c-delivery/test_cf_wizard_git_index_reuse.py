"""Persistent Git index, sparse previews, migration and failure boundaries."""
from collections import Counter
from contextlib import closing
from hashlib import sha1, sha256
import json
from pathlib import Path
import sqlite3
import unittest
from unittest.mock import patch

from cf_wizard.dependency_index import IndexReader, update_git_index
from cf_wizard.git_api import Blob, Tree, GitError
from cf_wizard.git_source import ExportSourceFactory
from cf_wizard.helper_worker import HELPER_SHA256


def blob(data):
    return Blob(sha1(f"blob {len(data)}\0".encode() + data).hexdigest(), len(data))


class MemoryGit:
    """Complete immediate trees and pinned blobs, shared by both provider contracts."""
    repository = "example/metadata"

    def __init__(self, bodies, provider="gitea"):
        self.provider = provider
        self.label = "GitHub" if provider == "github" else "Gitea"
        self.server = "https://github.com" if provider == "github" else "https://git.example.test"
        self.trees, self.revisions = {}, {}
        self.requests = Counter()
        self.add(bodies)

    def add(self, bodies):
        self.head = sha1(str(len(self.revisions)).encode()).hexdigest()
        self.revisions[self.head] = dict(bodies)
        return self.head

    def entries(self, revision, folder):
        prefix = folder + "/" if folder else ""
        bodies = self.revisions[revision]
        entries = {}
        for path, data in bodies.items():
            if not path.startswith(prefix):
                continue
            name, slash, tail = path[len(prefix):].partition("/")
            if name in entries:
                continue
            if slash:
                children = self.entries(revision, prefix + name)
                payload = json.dumps([(key, value.oid) for key, value in sorted(children.items())]).encode()
                oid = sha1(payload).hexdigest()
                self.trees[oid] = children
                entries[name] = Tree(oid)
            else:
                entries[name] = blob(data)
        return entries

    def branch_sha(self, branch):
        self.requests["head"] += 1
        return self.head

    def config_tree(self, revision, directory):
        self.requests["root_tree"] += 1
        return self.entries(revision, "")

    def tree(self, oid):
        self.requests["tree"] += 1
        return self.trees[oid]

    def file(self, revision, path, entry):
        self.requests["file"] += 1
        return self.revisions[revision][path.removeprefix("config/")]

    def files(self, revision, paths):
        self.requests["batch"] += 1
        self.requests["downloaded"] += len(paths)
        return {path: self.revisions[revision][path.removeprefix("config/")] for path in paths}


class GitIndexReuseTests(unittest.TestCase):
    def setUp(self):
        from test_cf_wizard import WizardTests
        self.case = WizardTests()
        self.case.setUp()
        self.addCleanup(self.case.doCleanups)
        self.wizard = self.case.indexed_wizard()
        self.factory = ExportSourceFactory(self.wizard.open_export, self.case.root / "Git")
        self.bodies = {path.relative_to(self.case.export).as_posix(): path.read_bytes()
                       for path in self.case.export.rglob("*") if path.is_file()}
        self.bodies["Documents/Заказ/Ext/ObjectModule.bsl"] = "// Первая версия\n".encode()
        self.client = MemoryGit(self.bodies)

    def open(self, client=None, branch="main"):
        return self.factory.git(client or self.client, branch, "config")

    def ready(self, source, path):
        reader = IndexReader.open(path, source.identity, HELPER_SHA256, inputs=source.git.index_inputs, pinned=True)
        if reader:
            reader.close()
        return reader is not None

    def test_same_inputs_reuse_bytes_across_restarts_and_bsl_revision_for_both_providers(self):
        for provider in ("gitea", "github"):
            with self.subTest(provider=provider):
                client = MemoryGit(self.bodies, provider)
                source = self.open(client)
                update = self.wizard.refresh_index(source, "")
                saved, stamp = update.path.read_bytes(), update.path.stat().st_mtime_ns
                changed = dict(self.bodies)
                changed["Documents/Заказ/Ext/ObjectModule.bsl"] = "// Вторая версия\n".encode()
                client.add(changed)
                newer = self.open(client)
                self.assertNotEqual(source.identity, newer.identity)
                self.assertEqual(source.git.index_inputs.fingerprint, newer.git.index_inputs.fingerprint)
                self.assertEqual(list(newer.root.rglob("*.xml")), [newer.root / "Configuration.xml"])
                before = client.requests.copy()
                with patch.object(newer.git, "ensure", side_effect=AssertionError("No XML IO during readiness")):
                    self.assertTrue(self.ready(newer, update.path))
                self.assertEqual(client.requests, before)
                self.assertEqual(update.path.read_bytes(), saved)
                self.assertEqual(update.path.stat().st_mtime_ns, stamp)
                restarted = self.open(client)
                self.assertTrue(self.ready(restarted, update.path))
                preview = self.wizard.preview(newer, "", {"Document.Заказ"}, str(self.case.output))
                self.addCleanup(self.wizard.discard, preview)
                self.assertIn("Catalog.Склады", {stub.name for stub in preview.dependencies})
                module = newer.root / "Documents/Заказ/Ext/ObjectModule.bsl"
                self.assertEqual(module.read_bytes(), changed["Documents/Заказ/Ext/ObjectModule.bsl"])
                with self.case.emulate_native():
                    self.assertTrue(self.wizard.build(preview, str(self.case.platform)).is_dir())

    def test_changed_descriptor_waits_for_button_and_only_reads_one_xml(self):
        source = self.open()
        first = self.wizard.refresh_index(source, "")
        saved = first.path.read_bytes()
        changed = dict(self.bodies)
        changed["Catalogs/Склады.xml"] += b"\n<!-- changed -->"
        self.client.add(changed)
        newer = self.open()
        self.assertFalse(self.ready(newer, first.path))
        self.assertEqual(first.path.read_bytes(), saved)
        with patch.object(newer.git, "ensure", wraps=newer.git.ensure) as ensure:
            update = self.wizard.refresh_index(newer, "")
        self.assertEqual({call.args[0] for call in ensure.call_args_list}, {"Catalogs/Склады.xml"})
        self.assertEqual((update.added, update.changed, update.removed, update.unchanged), (0, 1, 0, 1))
        self.assertIn("обработано XML 1", update.log.read_text(encoding="utf8"))
        self.assertTrue(self.ready(newer, update.path))

    def test_language_and_documents_outside_configuration_are_inputs_and_update_registrars(self):
        changed = dict(self.bodies)
        changed["Configuration.xml"] = changed["Configuration.xml"].replace(
            b"</ChildObjects>", b"<Language>English</Language></ChildObjects>")
        changed["Languages/English.xml"] = b"<MetaDataObject><Language><Properties><Name>English</Name></Properties></Language></MetaDataObject>"
        changed["Documents/Extra.xml"] = b"<MetaDataObject><Document><Properties><RegisterRecords><xr:Item>AccumulationRegister.Stock</xr:Item></RegisterRecords></Properties></Document></MetaDataObject>"
        self.client.add(changed)
        source = self.open()
        by_path = {entry.path: entry for entry in source.git.index_inputs.entries}
        self.assertEqual(by_path["Languages/English.xml"].roles, ("descriptor",))
        self.assertEqual(by_path["Documents/Extra.xml"].roles, ("registrar",))
        first = self.wizard.refresh_index(source, "")
        with closing(sqlite3.connect(first.path)) as db, db:
            self.assertIn(("Extra.xml",), db.execute("SELECT file FROM documents").fetchall())
        del changed["Documents/Extra.xml"]
        changed["Languages/English.xml"] += b"\n"
        self.client.add(changed)
        newer = self.open()
        self.assertFalse(self.ready(newer, first.path))
        update = self.wizard.refresh_index(newer, "")
        with closing(sqlite3.connect(update.path)) as db, db:
            self.assertNotIn(("Extra.xml",), db.execute("SELECT file FROM documents").fetchall())
        self.assertIn("обработано XML 1", update.log.read_text(encoding="utf8"))
        self.assertIn("удалено 1", update.log.read_text(encoding="utf8"))

    def test_legacy_migration_is_explicit_and_does_one_full_content_check(self):
        source = self.open()
        first = self.wizard.refresh_index(source, "")
        with closing(sqlite3.connect(first.path)) as db, db:
            db.execute("DELETE FROM metadata WHERE key LIKE 'git_%' OR key='indexed_revision'")
            db.execute("DROP TABLE git_inputs")
        self.assertTrue(self.ready(source, first.path))
        self.client.add(self.bodies)
        newer = self.open()
        self.assertFalse(self.ready(newer, first.path))
        update = self.wizard.refresh_index(newer, "")
        self.assertEqual((update.changed, update.added, update.unchanged), (0, 0, 2))
        self.assertIn("обработано XML 3", update.log.read_text(encoding="utf8"))
        self.assertTrue(self.ready(newer, first.path))

    def test_bad_blob_abort_and_concurrent_replacement_preserve_previous_index(self):
        import runpy
        source = self.open()
        first = self.wizard.refresh_index(source, "")
        saved = first.path.read_bytes()
        changed = dict(self.bodies)
        changed["Catalogs/Склады.xml"] += b"<!-- new -->"
        self.client.add(changed)
        newer = self.open()
        with patch.object(self.client, "files", return_value={"config/Catalogs/Склады.xml": b"bad"}):
            with self.assertRaises(ValueError):
                self.wizard.refresh_index(newer, "")
        self.assertEqual(first.path.read_bytes(), saved)
        helper = runpy.run_path(str(self.wizard.reader.helper))
        with self.assertRaisesRegex(ValueError, "stop"):
            update_git_index(first.path, newer, dict(newer.descriptors), helper, HELPER_SHA256,
                             lambda: (_ for _ in ()).throw(ValueError("stop")))
        self.assertEqual(first.path.read_bytes(), saved)
        def replace_elsewhere():
            first.path.write_bytes(saved + b"other writer")
        with self.assertRaisesRegex(ValueError, "другом окне"):
            update_git_index(first.path, newer, dict(newer.descriptors), helper, HELPER_SHA256, replace_elsewhere)
        self.assertEqual(first.path.read_bytes(), saved + b"other writer")

    def test_branch_provider_helper_and_corrupt_manifest_never_reuse(self):
        source = self.open()
        first = self.wizard.refresh_index(source, "")
        self.assertFalse(self.ready(self.open(branch="release"), first.path))
        self.assertFalse(self.ready(self.open(MemoryGit(self.bodies, "github")), first.path))
        self.assertIsNone(IndexReader.open(first.path, source.identity, "wrong", inputs=source.git.index_inputs))
        with closing(sqlite3.connect(first.path)) as db, db:
            db.execute("UPDATE git_inputs SET oid=? WHERE path='Configuration.xml'", ("f" * 40,))
        self.assertFalse(self.ready(source, first.path))

    def test_missing_descriptor_is_rejected_during_open(self):
        del self.bodies["Catalogs/Склады.xml"]
        self.client.add(self.bodies)
        with self.assertRaisesRegex(GitError, "XML-описание"):
            self.open()

    def test_defined_type_change_updates_closure_without_reindexing_consumer(self):
        from test_cf_wizard import MD, DOCUMENT
        bodies = dict(self.bodies)
        bodies['Configuration.xml'] = bodies['Configuration.xml'].replace(
            b'</ChildObjects>', '<DefinedType>Составной</DefinedType><Catalog>Второй</Catalog></ChildObjects>'.encode())
        bodies['Documents/Заказ.xml'] = bodies['Documents/Заказ.xml'].replace(
            'cfg:CatalogRef.Склады'.encode(), 'cfg:DefinedType.Составной'.encode())
        bodies['DefinedTypes/Составной.xml'] = (MD + '<DefinedType uuid="00000000-0000-0000-0000-000000000008">'
            '<Properties><Name>Составной</Name><Type><v8:Type>cfg:CatalogRef.Склады</v8:Type></Type></Properties>'
            '</DefinedType></MetaDataObject>').encode()
        bodies['Catalogs/Второй.xml'] = (MD + '<Catalog uuid="00000000-0000-0000-0000-000000000009">'
            '<Properties><Name>Второй</Name></Properties><ChildObjects/></Catalog></MetaDataObject>').encode()
        self.client.add(bodies)
        source = self.open()
        self.wizard.refresh_index(source, '')
        old = self.wizard.preview(source, '', {DOCUMENT}, str(self.case.output))
        self.addCleanup(self.wizard.discard, old)
        self.assertIn('Catalog.Склады', {stub.name for stub in old.dependencies})
        bodies['DefinedTypes/Составной.xml'] = bodies['DefinedTypes/Составной.xml'].replace(
            'cfg:CatalogRef.Склады'.encode(), 'cfg:CatalogRef.Второй'.encode())
        self.client.add(bodies)
        newer = self.open()
        with patch.object(newer.git, 'ensure', wraps=newer.git.ensure) as ensure:
            update = self.wizard.refresh_index(newer, '')
        self.assertEqual({call.args[0] for call in ensure.call_args_list}, {'DefinedTypes/Составной.xml'})
        preview = self.wizard.preview(newer, '', {DOCUMENT}, str(self.case.output))
        self.addCleanup(self.wizard.discard, preview)
        names = {stub.name for stub in preview.dependencies}
        self.assertIn('Catalog.Второй', names)
        self.assertNotIn('Catalog.Склады', names)

    def test_register_records_changes_registrar_links_and_output_stubs(self):
        from test_cf_wizard import MD, DOCUMENT
        register = 'AccumulationRegister.Stock'
        bodies = dict(self.bodies)
        bodies['Configuration.xml'] = bodies['Configuration.xml'].replace(
            b'</ChildObjects>', b'<AccumulationRegister>Stock</AccumulationRegister></ChildObjects>')
        bodies['AccumulationRegisters/Stock.xml'] = (MD + '<AccumulationRegister uuid="00000000-0000-0000-0000-000000000008">'
            '<Properties><Name>Stock</Name></Properties><ChildObjects/></AccumulationRegister></MetaDataObject>').encode()
        self.client.add(bodies)
        source = self.open()
        self.wizard.refresh_index(source, '')
        old = self.wizard.preview(source, '', {register}, str(self.case.output))
        self.addCleanup(self.wizard.discard, old)
        self.assertNotIn(DOCUMENT, {stub.name for stub in old.dependencies})
        record = b'<RegisterRecords><xr:Item xsi:type="xr:MDObjectRef">AccumulationRegister.Stock</xr:Item></RegisterRecords>'
        bodies['Documents/Заказ.xml'] = bodies['Documents/Заказ.xml'].replace(b'</Properties>', record+b'</Properties>', 1)
        self.client.add(bodies)
        newer = self.open()
        update = self.wizard.refresh_index(newer, '')
        with closing(sqlite3.connect(update.path)) as db:
            self.assertEqual(db.execute('SELECT target FROM registrar_links').fetchall(), [(register,)])
        preview = self.wizard.preview(newer, '', {register}, str(self.case.output))
        self.addCleanup(self.wizard.discard, preview)
        self.assertIn(DOCUMENT, {stub.name for stub in preview.dependencies})
        bodies['Documents/Заказ.xml'] = bodies['Documents/Заказ.xml'].replace(record, b'')
        self.client.add(bodies)
        removed = self.open()
        update = self.wizard.refresh_index(removed, '')
        with closing(sqlite3.connect(update.path)) as db:
            self.assertEqual(db.execute('SELECT target FROM registrar_links').fetchall(), [])
        final = self.wizard.preview(removed, '', {register}, str(self.case.output))
        self.addCleanup(self.wizard.discard, final)
        self.assertNotIn(DOCUMENT, {stub.name for stub in final.dependencies})

    def test_sparse_language_is_copied_to_build(self):
        from test_cf_wizard import MD, DOCUMENT
        bodies = dict(self.bodies)
        bodies['Configuration.xml'] = bodies['Configuration.xml'].replace(
            b'</ChildObjects>', b'<Language>English</Language></ChildObjects>')
        bodies['Languages/English.xml'] = (MD + '<Language uuid="00000000-0000-0000-0000-000000000008">'
            '<Properties><Name>English</Name><LanguageCode>en</LanguageCode></Properties></Language></MetaDataObject>').encode()
        self.client.add(bodies)
        source = self.open()
        self.wizard.refresh_index(source, '')
        bodies['Documents/Заказ/Ext/ObjectModule.bsl'] = b'// new'
        self.client.add(bodies)
        newer = self.open()
        self.assertFalse((newer.root/'Languages/English.xml').exists())
        preview = self.wizard.preview(newer, '', {DOCUMENT}, str(self.case.output))
        self.addCleanup(self.wizard.discard, preview)
        def check(stage):
            self.assertEqual((stage/'Languages/English.xml').read_bytes(), bodies['Languages/English.xml'])
        with self.case.emulate_native(inspect_stage=check):
            self.wizard.build(preview, str(self.case.platform))

    def test_os_writer_lock_rejects_second_writer_and_releases_after_error(self):
        from cf_wizard.git_index_store import _writer
        path = self.case.root/'lock.sqlite3'
        with self.assertRaisesRegex(ValueError, 'stop'):
            with _writer(path):
                with self.assertRaisesRegex(ValueError, 'другом окне'):
                    with _writer(path):
                        self.fail('second writer acquired the lock')
                raise ValueError('stop')
        with _writer(path):
            pass


if __name__ == "__main__":
    unittest.main()
