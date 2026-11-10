"""Synthetic full-Designer fixtures; real source-only helper, never 1C native."""
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("delivery_stage", Path(__file__).with_name("stage.py"))
stage = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(stage)
PINNED = Path(__file__).resolve().parents[2] / stage.HELPER
MODULE = "Documents/Order/Ext/ManagerModule.bsl"
DESCRIPTOR = "Documents/Order.xml"
PREFIX = '<MetaDataObject xmlns="http://v8.1c.ru/8.3/MDClasses" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xmlns:xr="http://v8.1c.ru/8.3/xcf/readable">'


class StageTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.write(stage.HELPER, PINNED.read_bytes())
        self.write("export/Configuration.xml", PREFIX + '<Configuration uuid="00000000-0000-0000-0000-000000000001"><Properties><Name>Fixture</Name></Properties><ChildObjects><Document>Order</Document></ChildObjects></Configuration></MetaDataObject>')
        self.write("export/" + DESCRIPTOR, PREFIX + '<Document uuid="00000000-0000-0000-0000-000000000002"><Properties><Name>Order</Name></Properties><ChildObjects/></Document></MetaDataObject>')
        self.write("export/" + MODULE, "\ufeffПроцедура Работа()\r\nКонецПроцедуры\r\n")
        self.write("export/ConfigDumpInfo.xml", "<synthetic-inventory/>")
        self.write("auxiliary-test.py", "# synthetic auxiliary source\n")
        self.config = {"export": "export", "expected_selected": ["Document.Order"],
                       "objects": ["Document.Order"], "files": [
                           {"source": "export/" + MODULE, "target": MODULE},
                           {"source": "export/" + DESCRIPTOR, "target": DESCRIPTOR}]}
        self.allowed = {"export/" + MODULE, "export/" + DESCRIPTOR, "auxiliary-test.py"}
        self.calls = []
        self.overrides = {}
        self.mutate = None
        self.run_id = 0

    def write(self, relative, data):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data.encode("utf-8") if isinstance(data, str) else data)
        return path

    def fake_runner(self, argv, **kwargs):
        self.calls.append((argv, kwargs))
        out = Path(argv[argv.index("--source-only") + 1])
        settings = Path(argv[argv.index("--settings") + 1])
        shutil.copytree(self.root / "export", out)
        settings.write_text("<Settings/>", encoding="utf-8")
        stubs = settings.with_name(settings.name + ".stubs.json")
        stubs.write_text(json.dumps([{"object": f"Catalog.Stub{i}"} for i in range(670)]), encoding="utf-8")
        report = {"ok": True, "selected": ["Document.Order"], "source": str(out),
                  "merge_settings": str(settings), "stubs_file": str(stubs), **self.overrides}
        if self.mutate:
            self.mutate(out)
        return subprocess.CompletedProcess(argv, 0, json.dumps(report).encode(), b"")

    def prepare(self, real=False):
        self.run_id += 1
        return stage.prepare_stage(self.config, self.root, self.root / f"run{self.run_id}", self.allowed,
                                   None if real else self.fake_runner)

    def test_real_pinned_source_only_preserves_production_files(self):
        self.config["files"] = self.config["files"][:1]
        self.allowed.remove("export/" + DESCRIPTOR)
        before = stage._inventory(self.root / "export")
        result = self.prepare(real=True)
        self.assertEqual(result["status"], "passed")
        self.assertEqual(result["selected"], ["Document.Order"])
        self.assertEqual(result["export_hashes"], before)
        self.assertIn("ConfigDumpInfo.xml", result["export_hashes"])
        self.assertEqual(result["roundtrip_modules"], [MODULE])
        self.assertEqual(result["roundtrip_xml"], [DESCRIPTOR])
        self.assertEqual(stage._inventory(self.root / "export"), before)
        for delivery in result["deliveries"]:
            self.assertEqual((Path(result["stage"]) / delivery["target"]).read_bytes(),
                             (self.root / delivery["source"]).read_bytes())
        self.assertEqual(json.loads((self.root / "run1/stage-receipt.json").read_text(encoding="utf-8")), result)
        self.assertTrue(Path(result["log"]).is_file())
        self.assertGreaterEqual(result["duration_seconds"], 0)
        self.assertFalse(list(self.root.rglob("__pycache__")))

    def test_typed_argv_and_auxiliary_sources_need_no_delivery(self):
        result = self.prepare()
        argv, kwargs = self.calls[0]
        self.assertIn("--source-only", argv)
        self.assertIn("-B", argv)
        self.assertNotIn("--out", argv)
        self.assertNotIn("--ibcmd", argv)
        self.assertFalse(kwargs["shell"])
        self.assertEqual(kwargs["env"]["GIT_OPTIONAL_LOCKS"], "0")
        self.assertEqual(Path(kwargs["env"]["TMP"]), self.root / "run1/tmp")
        self.assertEqual(len(json.loads(Path(result["stubs"]).read_text(encoding="utf-8"))), 670)
        self.assertNotIn("auxiliary-test.py", [v["source"] for v in result["deliveries"]])

    def test_since_target_methods_are_projected_without_shell(self):
        shutil.copytree(self.root / "export", self.root / "target")
        self.config.pop("objects")
        self.config.update(since="a" * 40, patch=True, target="target",
                           methods=["Document.Order.ManagerModule.Работа"])
        result = self.prepare()
        argv = self.calls[0][0]
        self.assertEqual(argv[argv.index("--since") + 1], "a" * 40)
        self.assertNotIn("--until", argv)
        self.assertEqual(argv[argv.index("--target") + 1], str(self.root / "target"))
        self.assertIn("--patch", argv)
        self.assertEqual(result["target_hashes"], stage._inventory(self.root / "target"))
        self.assertIn("target/" + MODULE, result["input_hashes"])

    def test_selected_owner_xml_subtree_is_covered_for_bsl_only(self):
        self.config["files"] = self.config["files"][:1]
        self.allowed.remove("export/" + DESCRIPTOR)
        self.write("export/Documents/Order/Forms/Edit.xml", "<synthetic-form-descriptor/>")
        self.write("export/Documents/Order/Forms/Edit/Ext/Form.xml", "<synthetic-form/>")
        result = self.prepare()
        self.assertEqual(result["roundtrip_xml"], [DESCRIPTOR,
                         "Documents/Order/Forms/Edit.xml", "Documents/Order/Forms/Edit/Ext/Form.xml"])
        self.assertEqual(result["roundtrip_modules"], [MODULE])

    def test_selected_owner_requires_exact_descriptor_identity(self):
        with self.assertRaisesRegex(stage.StageError, "matching descriptor"):
            stage.metadata_coverage(self.root / "export", ["Document.Missing"])
        self.write("export/Documents/Order.xml", PREFIX + '<Document><Properties><Name>Other</Name></Properties></Document></MetaDataObject>')
        self.config["files"] = self.config["files"][:1]
        self.allowed.remove("export/" + DESCRIPTOR)
        with self.assertRaisesRegex(stage.StageError, "matching descriptor"):
            self.prepare()

    def test_until_is_explicitly_unsupported_even_with_pinned_sha(self):
        self.config.pop("objects")
        self.config.update(since="a" * 40, until="b" * 40)
        with self.assertRaisesRegex(stage.StageError, "until is unsupported"):
            self.prepare()
        self.assertEqual(self.calls, [])

    def test_delivery_byte_mismatch_fails_with_receipt(self):
        self.mutate = lambda out: (out / MODULE).write_text("unrelated", encoding="utf-8")
        with self.assertRaisesRegex(stage.StageError, "differs from staged") as caught:
            self.prepare()
        self.assertEqual(caught.exception.receipt["status"], "failed")
        self.assertEqual(json.loads((self.root / "run1/stage-receipt.json").read_text(encoding="utf-8"))["status"], "failed")

    def test_report_selection_and_unresolved_states_block(self):
        for overrides in ({"selected": ["Document.Other"]}, {"conflicts": [1]},
                          {"patch_skipped": [1]}, {"deleted": [1]}, {"root_changed": True},
                          {"patched_whole": [{"object": "Document.Order", "reason": "synthetic"}]}):
            with self.subTest(overrides=overrides):
                self.overrides = overrides
                with self.assertRaises(stage.StageError):
                    self.prepare()
        self.config["expected_patched_whole"] = ["Document.Order"]
        self.overrides = {"patched_whole": [{"object": "Document.Order", "reason": "synthetic"}]}
        self.assertEqual(self.prepare()["status"], "passed")

    def test_all_export_files_are_immutable(self):
        self.mutate = lambda out: self.write("export/ConfigDumpInfo.xml", "changed")
        with self.assertRaisesRegex(stage.StageError, "inventory changed"):
            self.prepare()

    def test_helper_and_target_inputs_are_immutable(self):
        self.mutate = lambda out: self.write(stage.HELPER, "# mutated helper")
        with self.assertRaisesRegex(stage.StageError, "Pinned stage inputs changed"):
            self.prepare()
        self.write(stage.HELPER, PINNED.read_bytes())
        shutil.copytree(self.root / "export", self.root / "target")
        self.config.pop("objects")
        self.config.update(since="a" * 40, target="target")
        self.mutate = lambda out: self.write("target/new.xml", "<extra/>")
        with self.assertRaisesRegex(stage.StageError, "inventory changed"):
            self.prepare()

    def test_files_scope_mapping_and_coverage_are_required(self):
        original = self.config["files"]
        for files in ([], [original[0]], [{"source": "auxiliary-test.py", "target": "auxiliary-test.py"}],
                      [{"source": "export/" + MODULE, "target": "Documents/Other/Ext/ManagerModule.bsl"}],
                      [original[0], original[0]]):
            with self.subTest(files=files):
                self.config["files"] = files
                with self.assertRaises(stage.StageError):
                    self.prepare()
        self.config["files"] = original
        self.allowed = set()
        with self.assertRaisesRegex(stage.StageError, "outside scope"):
            self.prepare()
        self.assertEqual(self.calls, [])

    def test_unsafe_or_untyped_options_are_rejected_before_command(self):
        for key, value in (("ibcmd", "anything"), ("roundtrip_modules", []),
                           ("export", "../escape"), ("patch", "true"),
                           ("expected_selected", [])):
            with self.subTest(key=key):
                config = dict(self.config, **{key: value})
                with self.assertRaises(stage.StageError):
                    stage.prepare_stage(config, self.root, self.root / "run", self.allowed, self.fake_runner)
        self.config.pop("objects")
        self.config["since"] = "--unsafe-option"
        with self.assertRaisesRegex(stage.StageError, "Unsafe Git revision"):
            self.prepare()
        self.assertEqual(self.calls, [])

    def test_existing_output_and_reparse_input_are_refused(self):
        self.write("run1/staging/existing", "keep")
        with self.assertRaisesRegex(stage.StageError, "already exists"):
            self.prepare()
        original = Path.lstat
        flagged = self.root / "export" / MODULE
        def fake_lstat(path, *args, **kwargs):
            value = original(path, *args, **kwargs)
            return SimpleNamespace(st_mode=value.st_mode, st_file_attributes=1024) if path == flagged else value
        with patch.object(Path, "lstat", fake_lstat):
            with self.assertRaisesRegex(stage.StageError, "Symlink/reparse"):
                self.prepare()
        self.assertEqual(self.calls, [])


if __name__ == "__main__":
    unittest.main()
