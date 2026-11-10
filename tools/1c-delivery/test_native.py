"""Focused native CF tests. No 1C process or existing infobase is used."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock


_spec = importlib.util.spec_from_file_location("delivery_native", Path(__file__).with_name("native.py"))
native = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(native)


class FakeNative:
    def __init__(self, stage, action=None, fail=None):
        self.stage = stage
        self.action = action
        self.fail = fail
        self.calls = []

    def __call__(self, argv, **kwargs):
        self.calls.append((argv, kwargs))
        step = argv[2]
        if self.fail == step:
            return SimpleNamespace(returncode=102, stdout=b"native failure 102")
        values = dict(item[2:].split("=", 1) for item in argv if item.startswith("--"))
        if step == "create":
            Path(values["db-path"]).mkdir()
            Path(values["data"]).mkdir()
        elif step == "import":
            Path(values["out"]).write_bytes(b"isolated fake CF")
        elif step == "export":
            shutil.copytree(self.stage, Path(argv[-1]))
        if self.action:
            self.action(step, argv)
        return SimpleNamespace(returncode=0, stdout=f"{step} completed".encode())


class NativeBuildTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.stage = self.root / "stage"
        self.stage.mkdir()
        (self.stage / "Configuration.xml").write_text(
            '<m:Root xmlns:m="urn:metadata" xmlns:xs="http://www.w3.org/2001/XMLSchema">'
            '<m:Type>xs:string</m:Type><m:Text> value </m:Text></m:Root>', encoding="utf-8")
        (self.stage / "CommonModules").mkdir()
        self.module = "CommonModules/Test.bsl"
        (self.stage / self.module).write_bytes("Процедура Тест()\r\nКонецПроцедуры\r\n".encode("utf-8"))
        self.inventory = {path.relative_to(self.stage).as_posix(): hashlib.sha256(path.read_bytes()).hexdigest()
                          for path in self.stage.rglob("*") if path.is_file()}
        self.exe = self.root / "ibcmd.exe"
        self.exe.write_bytes(b"fake executable; never launched")
        self.work = self.root / "isolated-build"

    def build(self, runner=None, **overrides):
        arguments = dict(stage=self.stage, work_dir=self.work, ibcmd=self.exe,
                         platform_version="8.3.27.2214", configuration_name="TestConfiguration",
                         date_stamp="291026", module_paths=[self.module],
                         xml_paths=["Configuration.xml"], inventory=self.inventory,
                         command_runner=runner or FakeNative(self.stage))
        arguments.update(overrides)
        return native.build_cf(**arguments)

    def test_success_uses_exact_isolated_native_contract_and_receipt(self):
        runner = FakeNative(self.stage)
        receipt = self.build(runner)
        self.assertEqual(receipt["status"], "passed")
        self.assertTrue(receipt["stagingUnchanged"])
        self.assertFalse(receipt["existingDatabasesChanged"])
        self.assertFalse(receipt["runtimeAcceptance"])
        self.assertFalse(receipt["updateDBCfg"])
        self.assertEqual([call[0][1:3] for call in runner.calls],
                         [["infobase", "create"], ["config", "import"], ["config", "export"]])
        for argv, kwargs in runner.calls:
            self.assertEqual(argv[0], str(self.exe))
            self.assertFalse(kwargs["shell"])
            for item in argv:
                if item.startswith(("--db-path=", "--data=", "--out=", "--file=")):
                    self.assertTrue(Path(item.split("=", 1)[1]).is_relative_to(self.work))
            if os.name == "nt":
                self.assertEqual(kwargs["creationflags"], subprocess.CREATE_NO_WINDOW)
        self.assertEqual(runner.calls[1][0][-1], str(self.stage))
        self.assertEqual(runner.calls[2][0][-1], str(self.work / "readback"))
        self.assertTrue(any(item.startswith("--out=") for item in runner.calls[1][0]))
        self.assertTrue(any(item.startswith("--file=") for item in runner.calls[2][0]))
        for step in receipt["steps"]:
            self.assertEqual(step["exitCode"], 0)
            self.assertTrue(step["startedUtc"].endswith("Z"))
            self.assertTrue(step["finishedUtc"].endswith("Z"))
            self.assertGreaterEqual(step["monotonicEnd"], step["monotonicStart"])
            self.assertGreaterEqual(step["seconds"], 0)
            self.assertTrue(Path(step["log"]).is_file())
        self.assertEqual(json.loads((self.work / "build-receipt.json").read_text(encoding="utf-8")), receipt)
        self.assertEqual(hashlib.sha256(Path(receipt["file"]).read_bytes()).hexdigest(), receipt["sha256"])

    def test_empty_readback_coverage_is_rejected_without_native(self):
        runner = FakeNative(self.stage)
        with self.assertRaisesRegex(native.NativeBuildError, "at least one BSL or XML"):
            self.build(runner, module_paths=[], xml_paths=[])
        self.assertEqual(runner.calls, [])
        self.assertFalse(self.work.exists())

    def test_duplicate_readback_paths_are_rejected_without_native(self):
        runner = FakeNative(self.stage)
        for overrides in ({"module_paths": [self.module, self.module]},
                          {"xml_paths": ["Configuration.xml", "Configuration.xml"]}):
            with self.subTest(overrides=overrides), self.assertRaisesRegex(native.NativeBuildError, "duplicates"):
                self.build(runner, **overrides)
        self.assertEqual(runner.calls, [])
        self.assertFalse(self.work.exists())

    def test_readback_kind_and_inventory_link_are_required(self):
        runner = FakeNative(self.stage)
        for overrides in ({"module_paths": ["Configuration.xml"]},
                          {"xml_paths": [self.module]}, {"xml_paths": ["Absent.xml"]}):
            with self.subTest(overrides=overrides), self.assertRaisesRegex(native.NativeBuildError, "wrong kind"):
                self.build(runner, **overrides)
        self.assertEqual(runner.calls, [])
        self.assertFalse(self.work.exists())

    def test_single_bsl_or_single_xml_coverage_is_allowed(self):
        for kind, modules, xmls in (("bsl", [self.module], []), ("xml", [], ["Configuration.xml"])):
            with self.subTest(kind=kind):
                receipt = self.build(module_paths=modules, xml_paths=xmls, work_dir=self.root / kind)
                self.assertEqual(receipt["status"], "passed")

    def test_readback_paths_reject_non_string_lists(self):
        runner = FakeNative(self.stage)
        for paths in ((self.module,), [[self.module]], [None]):
            with self.subTest(paths=paths), self.assertRaisesRegex(native.NativeBuildError, "lists of strings"):
                self.build(runner, module_paths=paths)
        self.assertEqual(runner.calls, [])
        self.assertFalse(self.work.exists())

    def test_native_nonzero_stops_without_retry_and_checks_stage(self):
        runner = FakeNative(self.stage, fail="import")
        with self.assertRaisesRegex(native.NativeBuildError, "102") as caught:
            self.build(runner)
        self.assertEqual(len(runner.calls), 2)
        receipt = caught.exception.receipt
        self.assertEqual(receipt["status"], "failed")
        self.assertTrue(receipt["stagingUnchanged"])
        self.assertEqual(receipt["steps"][-1]["exitCode"], 102)
        self.assertIn("native failure", Path(receipt["steps"][-1]["log"]).read_text())
        self.assertTrue((self.work / "build-receipt.json").is_file())

    def test_native_timeout_stops_without_retry_and_saves_log(self):
        calls = []
        def timeout(argv, **kwargs):
            calls.append(argv)
            raise subprocess.TimeoutExpired(argv, 240, output=b"partial output")
        with self.assertRaisesRegex(native.NativeBuildError, "TimeoutExpired") as caught:
            self.build(timeout)
        self.assertEqual(len(calls), 1)
        self.assertTrue(caught.exception.receipt["stagingUnchanged"])
        self.assertIn("partial output", Path(caught.exception.receipt["steps"][0]["log"]).read_text())

    def test_changed_stage_is_rejected_after_native_commands(self):
        def change(step, argv):
            if step == "import":
                (self.stage / self.module).write_bytes(b"changed staging")
        with self.assertRaisesRegex(native.NativeBuildError, "Staging inventory differs") as caught:
            self.build(FakeNative(self.stage, action=change))
        self.assertFalse(caught.exception.receipt["stagingUnchanged"])

    def test_existing_work_directory_is_rejected_without_native(self):
        self.work.mkdir()
        sentinel = self.work / "existing.txt"
        sentinel.write_text("keep")
        runner = FakeNative(self.stage)
        with self.assertRaisesRegex(native.NativeBuildError, "must be new"):
            self.build(runner)
        self.assertEqual(runner.calls, [])
        self.assertEqual(sentinel.read_text(), "keep")

    def test_bsl_byte_change_is_rejected(self):
        def change(step, argv):
            if step == "export":
                (Path(argv[-1]) / self.module).write_bytes(b"wrong module")
        with self.assertRaisesRegex(native.NativeBuildError, "BSL readback differs") as caught:
            self.build(FakeNative(self.stage, action=change))
        self.assertTrue(caught.exception.receipt["stagingUnchanged"])

    def test_xml_meaningful_whitespace_change_is_rejected(self):
        def change(step, argv):
            if step == "export":
                path = Path(argv[-1]) / "Configuration.xml"
                path.write_text(path.read_text(encoding="utf-8").replace(" value ", "value"), encoding="utf-8")
        with self.assertRaisesRegex(native.NativeBuildError, "XML readback differs"):
            self.build(FakeNative(self.stage, action=change))

    def test_xml_prefix_changes_and_formatting_preserve_semantics(self):
        def change(step, argv):
            if step == "export":
                path = Path(argv[-1]) / "Configuration.xml"
                text = path.read_text(encoding="utf-8").replace("m:", "n:").replace("xmlns:m=", "xmlns:n=")
                text = text.replace("xmlns:xs=", "xmlns:schema=").replace("xs:string", "schema:string")
                text = text.replace("><n:Type>", ">\n  <n:Type>").replace("</n:Type><", "</n:Type>\n  <")
                path.write_text(text, encoding="utf-8")
        self.assertEqual(self.build(FakeNative(self.stage, action=change))["status"], "passed")

    def test_xml_qname_namespace_rebinding_is_rejected(self):
        def change(step, argv):
            if step == "export":
                path = Path(argv[-1]) / "Configuration.xml"
                path.write_text(path.read_text(encoding="utf-8").replace("http://www.w3.org/2001/XMLSchema", "urn:wrong"), encoding="utf-8")
        with self.assertRaisesRegex(native.NativeBuildError, "XML readback differs"):
            self.build(FakeNative(self.stage, action=change))

    def test_full_inventory_detects_added_and_missing_files_before_native(self):
        (self.stage / "unexpected.txt").write_text("not in receipt")
        runner = FakeNative(self.stage)
        with self.assertRaisesRegex(native.NativeBuildError, "added="):
            self.build(runner)
        self.assertEqual(runner.calls, [])
        self.assertFalse(self.work.exists())

    def test_path_traversal_and_windows_alternatives_are_rejected(self):
        for path in ("../escape.bsl", "nested/../../escape.bsl", "/absolute.bsl", "C:/escape.bsl",
                     "..\\escape.bsl", "name.bsl:stream", "NUL.bsl"):
            with self.subTest(path=path), self.assertRaises(native.NativeBuildError):
                self.build(inventory={path: "0" * 64}, module_paths=[])
        self.assertFalse(self.work.exists())

    def test_repository_metadata_cannot_be_used_for_native_work(self):
        metadata = self.root / ".git"
        metadata.mkdir()
        with self.assertRaisesRegex(native.NativeBuildError, "repository metadata"):
            self.build(work_dir=metadata / "new-build")
        self.assertEqual(list(metadata.iterdir()), [])

    def test_nonempty_cf_is_required(self):
        def empty(step, argv):
            if step == "import":
                output = next(item.split("=", 1)[1] for item in argv if item.startswith("--out="))
                Path(output).write_bytes(b"")
        with self.assertRaisesRegex(native.NativeBuildError, "nonempty CF") as caught:
            self.build(FakeNative(self.stage, action=empty))
        checks = caught.exception.receipt["checks"]
        self.assertEqual(next(check for check in checks if check["check"] == "readback")["status"], "failed")

    @unittest.skipUnless(os.name == "nt", "Designer batch commands require Windows")
    def test_designer_stops_when_batch_result_is_missing_or_failed(self):
        designer = self.root / "1cv8.exe"
        designer.touch()
        for name, status in (("missing", None), ("failed", "1")):
            with self.subTest(status=status):
                work = self.root / f"временная база {name}"
                def run(argv, **kwargs):
                    self.assertIn(f'File="{work / "db"}";', argv)
                    self.assertEqual(kwargs["executable"], str(designer))
                    self.assertEqual(kwargs["startupinfo"].wShowWindow, subprocess.SW_HIDE)
                    (work / "logs/01-create.out").write_text("Сообщение Конфигуратора", encoding="utf-8-sig")
                    if status is not None:
                        (work / "logs/01-create.result").write_text(status, encoding="utf-8-sig")
                    return SimpleNamespace(returncode=0, stdout=b"")

                runner = mock.Mock(side_effect=run)
                with self.assertRaisesRegex(native.NativeBuildError, r"01-create returned /DumpResult") as caught:
                    self.build(runner, work_dir=work, ibcmd=designer, backend="designer")
                runner.assert_called_once()
                self.assertTrue(caught.exception.receipt["stagingUnchanged"])
                self.assertEqual(caught.exception.receipt["steps"][0]["resultCode"], status)
                self.assertIn("Сообщение Конфигуратора", (work / "logs/01-create.log").read_text(encoding="utf-8"))
                self.assertEqual(list((work / "output").iterdir()), [])

    def test_stage_is_checked_after_a_native_failure_too(self):
        def failed(argv, **kwargs):
            (self.stage / self.module).write_bytes(b"changed before native failure")
            return SimpleNamespace(returncode=102, stdout=b"native failure")
        with self.assertRaisesRegex(native.NativeBuildError, "Staging inventory differs") as caught:
            self.build(failed)
        self.assertFalse(caught.exception.receipt["stagingUnchanged"])
        self.assertEqual(len(caught.exception.receipt["steps"]), 1)

    def test_work_directory_under_stage_is_rejected(self):
        with self.assertRaisesRegex(native.NativeBuildError, "separate from stage"):
            self.build(work_dir=self.stage / "build")
        self.assertFalse((self.stage / "build").exists())

    def test_symlink_stage_entry_is_rejected_when_supported(self):
        outside = self.root / "outside.bsl"
        outside.write_bytes(b"outside")
        try:
            (self.stage / "linked.bsl").symlink_to(outside)
        except (OSError, NotImplementedError):
            # Windows may forbid symlink creation. Exercise the same native
            # reparse-point guard with a simulated FILE_ATTRIBUTE_REPARSE_POINT.
            linked = self.stage / "linked.bsl"
            linked.write_bytes(b"simulated reparse point")
            original_lstat = Path.lstat
            def reparse_lstat(path, *args, **kwargs):
                info = original_lstat(path, *args, **kwargs)
                if path == linked:
                    return SimpleNamespace(st_mode=info.st_mode, st_file_attributes=0x400)
                return info
            with mock.patch.object(Path, "lstat", reparse_lstat):
                with self.assertRaisesRegex(native.NativeBuildError, "Symlink or reparse"):
                    self.build()
        else:
            with self.assertRaisesRegex(native.NativeBuildError, "Symlink or reparse"):
                self.build()
        self.assertFalse(self.work.exists())


if __name__ == "__main__":
    unittest.main()
