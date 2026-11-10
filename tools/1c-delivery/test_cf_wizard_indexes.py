"""Focused checks for the manual ConfigDumpInfo workflow."""

import json
from pathlib import Path
import shutil
import tempfile
import tkinter as tk
import unittest
from unittest.mock import patch
import xml.etree.ElementTree as ET

from cf_wizard.dump_index_io import NAMESPACE, DesignerIndexFiles
from cf_wizard.dump_index_view import DumpIndexPage
from cf_wizard.dump_indexes import CHECK_UPDATE, EXPORT_LIST, INDEX_NAME, IndexWizard


CONFIGURATION = "Configuration.Тест"
ROOT_ID = "00000000-0000-0000-0000-000000000001"


class IndexTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix="Индексы CF ")
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
        self.working = self.root / "Рабочая выгрузка"
        self.working.mkdir()
        self.output = self.root / "Результат"
        self.output.mkdir()
        (self.working / "Configuration.xml").write_text(
            '<MetaDataObject><Configuration uuid="' + ROOT_ID + '"><Properties><Name>Тест</Name>'
            '</Properties><ChildObjects/></Configuration></MetaDataObject>', encoding="utf-8")
        self.baseline = self.working / INDEX_NAME
        self.fresh = self.root / "Свежий индекс.xml"
        self.wizard = IndexWizard(DesignerIndexFiles())

    def write_index(self, path, records=(), *, version="2.20", identity=ROOT_ID):
        root = ET.Element(NAMESPACE + "ConfigDumpInfo", format="Hierarchical", version=version)
        entries = ET.SubElement(root, NAMESPACE + "ConfigVersions")
        for address, uid, value in ((CONFIGURATION, identity, "version"), *records):
            attrs = {"name": address, "id": uid}
            if value is not None:
                attrs["configVersion"] = value
            ET.SubElement(entries, NAMESPACE + "Metadata", attrs)
        ET.ElementTree(root).write(path, encoding="utf-8", xml_declaration=True)

    def compare(self, baseline=None):
        return self.wizard.compare(self.working, str(self.output), str(baseline or self.baseline), str(self.fresh))

    def test_first_export_requires_an_empty_folder(self):
        empty = self.root / "Первая выгрузка"
        empty.mkdir()
        self.assertEqual(self.wizard.first_export(empty), f'/DumpConfigToFiles "{empty}" -Format Hierarchical')
        with self.assertRaisesRegex(ValueError, "пустую папку"):
            self.wizard.first_export(self.working)

    def test_export_key_points_to_empty_directory_separate_from_working_index(self):
        self.write_index(self.baseline)
        before = self.baseline.read_bytes()
        folder = self.root / "Выбранная папка для индекса"
        folder.mkdir()
        capture = self.wizard.prepare_export(self.working, str(folder))
        self.assertEqual(capture.index, folder / INDEX_NAME)
        self.assertEqual(list(folder.iterdir()), [])
        self.assertEqual(capture.arguments,
                         f'/DumpConfigToFiles "{folder}" -Format Hierarchical -configDumpInfoOnly')
        capture.index.write_bytes(before)
        with self.assertRaisesRegex(ValueError, "пустую папку"):
            self.wizard.prepare_export(self.working, str(folder))
        self.assertEqual(capture.index.read_bytes(), before)
        self.assertEqual(self.baseline.read_bytes(), before)

    def test_diff_lists_changed_parts_and_context_and_reports_removals(self):
        self.write_index(self.baseline, (
            ("CommonModule.Обмен", "cm", "1"),
            ("CommonModule.Обмен.Module", "cm.0", "1"),
            ("Catalog.СтароеИмя", "rename", "1"),
            ("Constant.Удалённая", "deleted", "1"),
            ("Document.Заказ", "order", "1"),
            ("Document.Заказ.Attribute.Склад", "warehouse", None),
        ))
        self.write_index(self.fresh, (
            ("CommonModule.Обмен", "cm", "1"),
            ("CommonModule.Обмен.Module", "cm.0", "2"),
            ("Catalog.НовоеИмя", "rename", "1"),
            ("Document.Заказ", "order", "1"),
            ("Document.Заказ.Attribute.Склад", "new-warehouse", None),
            ("Document.Заказ.Form.Форма", "form", "1"),
            ("Document.Заказ.Form.Форма.Form", "form.0", "1"),
        ))
        before = self.baseline.read_bytes()
        result = self.compare()
        names = (result.directory / "dump-objects.txt").read_text(encoding="utf-8-sig").splitlines()
        self.assertEqual(names, ["Catalog.НовоеИмя", "CommonModule.Обмен.Module", CONFIGURATION,
                                 "Document.Заказ", "Document.Заказ.Form.Форма"])
        self.assertEqual(list((result.directory / "delta").iterdir()), [])
        self.assertIn(f'"{result.directory / "delta"}"', result.commands[EXPORT_LIST])
        self.assertIn("-listFile", result.commands[EXPORT_LIST])
        logs = result.directory / "logs"
        self.assertTrue(logs.is_dir())
        self.assertIn(f'/Out "{logs / "list-file.log"}"', result.commands[EXPORT_LIST])
        self.assertIn(f'/DumpResult "{logs / "list-file.result.txt"}"', result.commands[EXPORT_LIST])
        self.assertEqual(result.update_check.changes.parent, logs)
        self.assertEqual(result.update_check.result.parent, logs)
        self.assertEqual(result.update_check.log.parent, logs)
        self.assertEqual(tuple(result.commands), (EXPORT_LIST, CHECK_UPDATE))
        self.assertIn("-getChanges", result.commands[CHECK_UPDATE])
        self.assertIn(f'/DumpResult "{result.update_check.result}"', result.commands[CHECK_UPDATE])
        self.assertIn(f'/Out "{result.update_check.log}"', result.commands[CHECK_UPDATE])
        self.assertNotIn("-update", " ".join(result.commands.values()))
        report = json.loads((result.directory / "changes.json").read_text(encoding="utf-8"))
        self.assertIn(("renamed", "Catalog.СтароеИмя", "Catalog.НовоеИмя"),
                      [(item["kind"], item["before"], item["after"]) for item in report["changes"]])
        self.assertIn(("removed", "Constant.Удалённая", None),
                      [(item["kind"], item["before"], item["after"]) for item in report["changes"]])
        self.assertEqual(self.baseline.read_bytes(), before)

    def test_external_baseline_does_not_offer_working_folder_update(self):
        self.write_index(self.baseline, (("CommonModule.Обмен", "cm", "1"),))
        self.write_index(self.fresh, (("CommonModule.Обмен", "cm", "2"),))
        external = self.root / "Базовый индекс.xml"
        shutil.copyfile(self.baseline, external)
        self.assertEqual(tuple(self.compare(external).commands), (EXPORT_LIST,))

    def test_removed_only_has_report_and_no_listfile_command(self):
        self.write_index(self.baseline, (("Constant.Удалённая", "deleted", "1"),))
        self.write_index(self.fresh)
        result = self.compare()
        self.assertFalse(result.diff.dump_objects)
        self.assertEqual(result.diff.changes[0].kind, "removed")
        self.assertEqual(tuple(result.commands), (CHECK_UPDATE,))

    def test_update_key_requires_a_successful_native_report_with_changes(self):
        self.write_index(self.baseline, (("CommonModule.Обмен", "cm", "1"),))
        self.write_index(self.fresh, (("CommonModule.Обмен", "cm", "2"),))
        plan = self.compare()
        check = plan.update_check
        with self.assertRaisesRegex(ValueError, "дождитесь файла результата"):
            self.wizard.update_arguments(plan)

        check.changes.write_text("FullDump\n", encoding="utf-8-sig")
        check.result.write_text("0", encoding="utf-16")
        with self.assertRaisesRegex(ValueError, "FullDump"):
            self.wizard.update_arguments(plan)

        check.changes.write_text("Modified: CommonModule.Обмен\n", encoding="utf-8-sig")
        check.result.write_text("1", encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "код 1"):
            self.wizard.update_arguments(plan)

        check.result.write_text("0", encoding="utf-16")
        logs = plan.directory / "logs"
        self.assertEqual(self.wizard.update_arguments(plan),
                         f'/DumpConfigToFiles "{self.working}" -Format Hierarchical -update '
                         f'/Out "{logs / "update.log"}" /DumpResult "{logs / "update.result.txt"}"')
        check.changes.write_text("Unexpected: CommonModule.Обмен\n", encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "Формат ответа 1С"):
            self.wizard.update_arguments(plan)

        check.changes.write_text("", encoding="utf-8")
        self.assertIsNone(self.wizard.update_arguments(plan))
        self.write_index(self.baseline, (("CommonModule.Обмен", "cm", "3"),))
        with self.assertRaisesRegex(ValueError, "Индекс рабочей папки изменился"):
            self.wizard.update_arguments(plan)

    def test_page_shows_list_file_and_command_after_comparison(self):
        address = "Catalog.Покупатели"
        identity = "00000000-0000-0000-0000-000000000002"
        self.write_index(self.baseline, [(address, identity, "before")])
        self.write_index(self.fresh, [(address, identity, "after")])
        try:
            root = tk.Tk()
        except tk.TclError as exc:
            self.skipTest(str(exc))
        root.withdraw()
        self.addCleanup(root.destroy)
        page = DumpIndexPage(root, self.wizard)
        page.working.set(str(self.working))
        page.current.set(str(self.fresh))
        page.destination.set(str(self.output))

        def run_now(_widget, work, completed):
            completed(work(), None)

        with (patch("cf_wizard.dump_index_view.run_in_background", side_effect=run_now),
              patch.object(root, "clipboard_clear") as clear_clipboard):
            page.compare()
            self.assertEqual(page.arguments.get(), page.plan.commands[EXPORT_LIST])
            self.assertTrue(page.list_button.instate(["!disabled"]))
            self.assertIn(str(page.plan.object_list), page.status.get())
            self.assertEqual(page.plan.object_list.read_text(encoding="utf-8-sig").splitlines(), [address])
            self.write_index(self.fresh)
            page.compare()
            self.assertEqual(page.arguments.get(), "")
            self.assertTrue(page.list_button.instate(["disabled"]))
            clear_clipboard.assert_not_called()

    def test_incompatible_indexes_and_output_inside_source_are_rejected(self):
        self.write_index(self.baseline)
        self.write_index(self.fresh, version="2.19")
        with self.assertRaisesRegex(ValueError, "Версии формата"):
            self.compare()
        self.write_index(self.fresh, identity="different")
        with self.assertRaisesRegex(ValueError, "разным конфигурациям"):
            self.compare()
        with self.assertRaisesRegex(ValueError, "вне рабочей XML-выгрузки"):
            self.wizard.prepare_export(self.working, str(self.working))
        self.assertEqual(list(self.output.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
