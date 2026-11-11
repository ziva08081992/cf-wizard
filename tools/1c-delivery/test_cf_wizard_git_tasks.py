"""Gitea task filtering over the real catalogue reader and synthetic responses."""

from concurrent.futures import CancelledError
from datetime import timedelta
from io import BytesIO
from pathlib import Path
from tempfile import TemporaryDirectory
from threading import Event
import unittest
from unittest.mock import Mock, patch
from urllib.error import HTTPError

from cf_wizard.adapters import DesignerExport
from cf_wizard.application import GitTaskQuery, Wizard
from cf_wizard.gitea import GiteaClient, GiteaError, GitFiles


REVISION = "a" * 40
MATCH = "b" * 40
ANOTHER = "c" * 40
OTHER = "d" * 40


def commit(sha, message):
    return {"sha": sha, "commit": {"message": message}}


def detail(sha, *paths):
    return {"sha": sha, "files": [{"filename": path} for path in paths]}


class GitTaskTests(unittest.TestCase):
    def setUp(self):
        directory = TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.client = GiteaClient("https://gitea.example.test", "example/metadata", "synthetic-token")
        files = GitFiles(self.client, "exports/config", "release", REVISION, {}, Path(directory.name))
        files.root.mkdir(parents=True)
        (files.root / "Configuration.xml").write_text(
            "<MetaDataObject><Configuration><Properties><Name>Sample</Name></Properties>"
            "<ChildObjects><Catalog>Items</Catalog><Catalog>Renamed</Catalog>"
            "<Document>Manual</Document><Language>Russian</Language></ChildObjects>"
            "</Configuration></MetaDataObject>", encoding="utf-8",
        )
        self.wizard = Wizard(DesignerExport(), Mock())
        self.source = self.wizard.open_export(str(files.root)).with_git(files)
        self.stop = Event()
        self.report = Mock()

    def search(self, query=None):
        return self.wizard.find_git_task(
            self.source, query or GitTaskQuery.create("erp-123", False), self.stop, self.report,
        )

    def test_pinned_history_with_small_pages_and_exact_task_key(self):
        query = GitTaskQuery.create(" erp-123 ", False)
        self.assertEqual(query.until - query.since, timedelta(days=90))
        responses = [
            [commit(OTHER, "ERP-1234; XERP-123; ERP-123_test")],
            [commit(MATCH, "Подготовка\nИсправление (erp-123)."), commit(ANOTHER, "ERP-123; ERP-123")],
            [],
            detail(MATCH, "exports/config/Catalogs/Items/Ext/ObjectModule.bsl",
                   "exports/config/Catalogs/Items/Forms/Removed/Ext/Form.xml",
                   "exports/config/Catalogs/Old.xml", "exports/config/Configuration.xml",
                   "exports/config-archive/Catalogs/Items.xml", "docs/notes.md"),
            detail(ANOTHER, "exports/config/Catalogs/Items/Ext/ObjectModule.bsl",
                   "exports/config/Catalogs/Renamed.xml", "exports/config/Languages/Russian.xml"),
        ]
        with patch.object(self.client, "_json", side_effect=responses) as request:
            result = self.search(query)
        self.assertEqual(result.query.key, "ERP-123")
        self.assertEqual(result.objects, {"Catalog.Items", "Catalog.Renamed"})
        self.assertEqual((result.commits, result.files, result.unmapped), (2, 6, 3))
        self.assertEqual([entry.args[0] for entry in request.call_args_list], [
            "commits", "commits", "commits", f"git/commits/{MATCH}", f"git/commits/{ANOTHER}",
        ])
        for page, entry in enumerate(request.call_args_list[:3], 1):
            self.assertEqual(entry.args[1], {
                "sha": REVISION, "page": page, "limit": 50, "since": query.since.isoformat(),
                "until": query.until.isoformat(), "files": "false", "stat": "false", "verification": "false",
            })
        for entry in request.call_args_list[3:]:
            self.assertEqual(entry.args[1], {"files": "true", "stat": "false", "verification": "false"})
        self.assertEqual([entry.args[0] for entry in self.report.call_args_list], [1, 3])

    def test_all_history_without_matches_or_dates(self):
        with patch.object(self.client, "_json", return_value=[]) as request:
            result = self.search(GitTaskQuery.create("ERP-123", True))
        self.assertEqual((result.commits, result.files, result.unmapped, result.objects), (0, 0, 0, set()))
        self.assertNotIn("since", request.call_args.args[1])
        self.assertNotIn("until", request.call_args.args[1])

    def test_repeated_page_and_page_limit_fail_without_reading_diffs(self):
        for pages, message in (
            ([[commit(MATCH, "ERP-123")]] * 2, "повторила коммит"),
            ([[commit(MATCH, "ERP-123")], [commit(OTHER, "Other")]], "предел страниц"),
        ):
            with self.subTest(message=message):
                with (patch("cf_wizard.gitea.MAX_COMMIT_PAGES", 2),
                      patch.object(self.client, "_json", side_effect=pages) as request):
                    with self.assertRaisesRegex(GiteaError, message):
                        self.search()
                self.assertEqual([entry.args[0] for entry in request.call_args_list], ["commits", "commits"])

    def test_incomplete_commit_data_fails_instead_of_returning_zero(self):
        for response in (
            {"sha": MATCH, "files": None}, detail(OTHER, "exports/config/Catalogs/Items.xml"),
            detail(MATCH, "exports/config/Catalogs/Items/../Old.xml"),
        ):
            with self.subTest(response=response), patch.object(
                self.client, "_json", side_effect=[[commit(MATCH, "ERP-123")], [], response],
            ):
                with self.assertRaises(GiteaError):
                    self.search()
        with patch.object(self.client, "_json", return_value=[commit(MATCH, None)]):
            with self.assertRaisesRegex(GiteaError, "не хватает"):
                self.search()

    def test_empty_files_on_a_valid_matching_commit(self):
        with patch.object(self.client, "_json", side_effect=[[commit(MATCH, "ERP-123")], [], detail(MATCH)]):
            result = self.search()
        self.assertEqual((result.commits, result.files, result.objects), (1, 0, set()))

    def test_transport_failures_hide_response_and_credentials(self):
        error = HTTPError("https://gitea.example.test", 401, "synthetic-token", {}, BytesIO(b"secret body"))
        for cause, message in ((error, "HTTP 401"), (OSError("synthetic-token"), "Не удалось прочитать")):
            with self.subTest(message=message), patch.object(self.client._opener, "open", side_effect=cause):
                with self.assertRaisesRegex(GiteaError, message) as caught:
                    self.search()
                self.assertNotIn("synthetic-token", str(caught.exception))
                self.assertNotIn("secret body", str(caught.exception))

    def test_cancel_after_a_page_stops_before_the_next_request(self):
        self.report.side_effect = lambda count: self.stop.set()
        with patch.object(self.client, "_json", return_value=[commit(MATCH, "ERP-123")]) as request:
            with self.assertRaises(CancelledError):
                self.search()
        request.assert_called_once()

    def test_cancel_during_a_file_list_stops_before_the_next_commit(self):
        responses = iter([[commit(MATCH, "ERP-123"), commit(ANOTHER, "ERP-123")], [], detail(MATCH)])

        def receive(endpoint, *_):
            if endpoint.startswith("git/"):
                self.stop.set()
            return next(responses)

        with patch.object(self.client, "_json", side_effect=receive) as request:
            with self.assertRaises(CancelledError):
                self.search()
        self.assertEqual([entry.args[0] for entry in request.call_args_list], [
            "commits", "commits", f"git/commits/{MATCH}",
        ])

    def test_invalid_key_is_rejected_without_request(self):
        for value in ("", "ERP-123, ERP-124", "ERP-123/abc", "ERP", "Я-123"):
            with self.subTest(value=value), self.assertRaises(ValueError):
                GitTaskQuery.create(value, False)
