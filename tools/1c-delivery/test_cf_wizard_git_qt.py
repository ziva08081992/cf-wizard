"""Native connection form and WebEngine flow, isolated from user credentials."""
import json
from datetime import datetime, timedelta
from threading import Event
import os
from pathlib import Path
import time
import unittest
from unittest.mock import Mock, patch
from urllib.parse import parse_qs, urlsplit
try:
    from PySide6.QtWidgets import QApplication, QDialog, QMessageBox
    QT = True
except ImportError:
    QT = False


@unittest.skipUnless(QT, 'PySide6 is available in the dedicated build venv')
class GitQtTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.app = QApplication.instance() or QApplication([])

    def settings(self, **updates):
        return dict(provider='gitea',server='https://gitea.example.test',repository='example/metadata',
                    directory='config',branch='main',cache=str(self.case.root/'Cache'),interval=0,**updates)

    def setUp(self):
        from test_cf_wizard import WizardTests
        self.case=WizardTests(); self.case.setUp(); self.addCleanup(self.case.doCleanups)

    def wait(self, predicate, timeout=25):
        until=time.monotonic()+timeout
        while time.monotonic()<until:
            self.app.processEvents()
            if predicate(): return
            time.sleep(.01)
        self.fail('Qt operation did not finish within test timeout')

    def js(self, window, expression):
        result=[]
        window.view.page().runJavaScript(expression,result.append)
        self.wait(lambda: bool(result))
        return result[0]

    def test_native_provider_switch_clears_secret_and_deletes_edited_target(self):
        from cf_wizard.qt_git_dialog import GitConnectionDialog
        forget=Mock(); dialog=GitConnectionDialog(None,self.settings(),['main'],forget)
        dialog.secret.setText('gitea-private-value'); dialog.remember.setChecked(True)
        dialog.provider.setCurrentIndex(dialog.provider.findData('github'))
        self.assertEqual(dialog.secret.text(),''); self.assertFalse(dialog.remember.isChecked())
        self.assertEqual(dialog.branch.count(),0)
        self.assertEqual(dialog.fields['server'].text(),'https://github.com')
        dialog.fields['server'].setText('https://api.github.com')
        dialog._delete_token()
        self.assertEqual(forget.call_args.args[0]['provider'],'github')
        self.assertEqual(forget.call_args.args[0]['server'],'https://api.github.com')
        dialog.provider.setCurrentIndex(dialog.provider.findData('gitea'))
        self.assertEqual(dialog.fields['server'].text(),'https://gitea.example.test')
        dialog.close(); dialog.deleteLater()

    def check_choice_menus(self, window, http):
        from PySide6.QtCore import QPoint, Qt
        from PySide6.QtTest import QTest
        # No platform-rendered HTML select may bypass the shared dark menu.
        self.assertEqual(self.js(window, "document.querySelectorAll('select').length"), 0)
        self.assertEqual(json.loads(self.js(window, "JSON.stringify(Array.from(document.querySelectorAll('.cf-choice-menu')).map(m => getComputedStyle(m).backgroundColor))")),
                         ['rgb(43, 45, 48)'] * 3)
        self.assertEqual(self.js(window, "document.querySelector('#cf-git-task-period').value"), 'recent')
        self.js(window, "document.querySelector('#cf-git-task-key').value='ERP-123'; document.querySelector('#cf-git-task-key').dispatchEvent(new Event('input'))")
        period = "document.querySelector('#cf-git-task-period')"
        menu = "document.querySelector('#cf-git-task-period-options')"
        self.js(window, period + '.click()')
        self.assertFalse(self.js(window, menu + '.hidden'))
        # Hover must move one highlight, without changing the selected period.
        for value in ('all', 'recent', 'all'):
            option = menu + f".querySelector('[value={value}]')"
            xy = json.loads(self.js(window, "JSON.stringify((() => {const r=" + option + ".getBoundingClientRect(); return [r.x+r.width/2,r.y+r.height/2];})())"))
            QTest.mouseMove(window.view.focusProxy(), QPoint(round(xy[0]), round(xy[1])))
            self.wait(lambda: self.js(window, 'document.activeElement.value') == value)
            self.assertEqual(self.js(window, menu + ".querySelectorAll(':focus').length"), 1)
            self.assertEqual(self.js(window, period + '.value'), 'recent')
            self.assertEqual(self.js(window, 'getComputedStyle(document.activeElement).backgroundColor'), 'rgb(39, 59, 96)')
            self.assertNotEqual(self.js(window, 'getComputedStyle(document.activeElement).color'), 'rgb(0, 0, 0)')
        # Commit the hovered option with the keyboard, then verify real search queries.
        QTest.keyClick(window.view.focusProxy(), Qt.Key.Key_Return)
        self.wait(lambda: self.js(window, period + '.value') == 'all')
        for value in ('all', 'recent'):
            if value == 'recent':
                self.js(window, period + '.focus()')
                QTest.keyClick(window.view.focusProxy(), Qt.Key.Key_Down)
                self.assertEqual(self.js(window, 'document.activeElement.value'), 'all')
                QTest.keyClick(window.view.focusProxy(), Qt.Key.Key_Home)
                QTest.keyClick(window.view.focusProxy(), Qt.Key.Key_Return)
            self.assertEqual(self.js(window, menu + ".querySelector('[aria-checked=true]').value"), value)
            self.assertEqual(self.js(window, "document.querySelector('#cf-git-task-period-value').textContent"),
                             'Вся история' if value == 'all' else 'Последние 90 дней')
            before = len(http.requests)
            self.js(window, "document.querySelector('[data-action=search-git-task]').click()")
            self.wait(lambda: window.bridge.task is None and len(http.requests) > before)
            queries = [parse_qs(urlsplit(r.full_url).query) for r in http.requests[before:]
                       if urlsplit(r.full_url).path.endswith('/commits')]
            self.assertTrue(queries)
            self.assertEqual('since' in queries[0], value == 'recent')
            self.wait(lambda: self.js(window, period + '.disabled') is False)
        self.check_custom_task_period(window, http)
        self.js(window, period + '.click()')
        QTest.keyClick(window.view.focusProxy(), Qt.Key.Key_Escape)
        self.wait(lambda: self.js(window, menu + '.hidden') is True)
        self.assertEqual(self.js(window, 'document.activeElement.id'), 'cf-git-task-period')
        # Leave the metadata tree unfiltered for the index/preview flow below.
        self.js(window, "document.querySelector('[data-action=clear-git-task]').click()")
        self.wait(lambda: window.bridge.snapshot()['gitTaskResult'] is None)

    def check_custom_task_period(self, window, http):
        from PySide6.QtCore import Qt
        from PySide6.QtTest import QTest
        period = "document.querySelector('#cf-git-task-period')"
        menu = "document.querySelector('#cf-git-task-period-options')"
        days = "document.querySelector('#cf-git-task-days')"
        find = "document.querySelector('[data-action=search-git-task]')"
        self.js(window, period + ".click(); " + menu + ".querySelector('[value=custom]').click()")
        self.assertFalse(self.js(window, "document.querySelector('[data-git-task-days]').hidden"))
        self.js(window, days + '.focus()')
        QTest.keyClick(window.view.focusProxy(), Qt.Key.Key_A, Qt.KeyboardModifier.ControlModifier)
        QTest.keyClicks(window.view.focusProxy(), '14')
        self.wait(lambda: self.js(window, days + '.value') == '14')
        started, release = Event(), Event()
        original_open = http.open
        def delayed(request, *args, **kwargs):
            if urlsplit(request.full_url).path.endswith('/commits'):
                started.set()
                if not release.wait(10): raise RuntimeError('Synthetic HTTP release timed out')
            return original_open(request, *args, **kwargs)
        before = len(http.requests)
        with patch.object(window.bridge.git_client._opener, 'open', side_effect=delayed):
            QTest.keyClick(window.view.focusProxy(), Qt.Key.Key_Return)
            try:
                self.wait(started.is_set)
                self.wait(lambda: self.js(window, days + '.disabled') is True)
                self.assertTrue(self.js(window, period + '.disabled'))
            finally:
                release.set()
            self.wait(lambda: window.bridge.task is None and len(http.requests) > before)
        params = [parse_qs(urlsplit(r.full_url).query) for r in http.requests[before:]
                  if urlsplit(r.full_url).path.endswith('/commits')][0]
        self.assertEqual(datetime.fromisoformat(params['until'][0])-datetime.fromisoformat(params['since'][0]),
                         timedelta(days=14))
        self.assertEqual(params['sha'], [window.bridge.catalog.git.revision])
        self.wait(lambda: self.js(window, days + '.disabled') is False)
        previous = window.bridge.git_task_filter
        selected = window.bridge.selected.copy()
        before = len(http.requests)
        for value in ('', '0', '-2', '1.5', '1e50'):
            self.js(window, days + '.value=' + json.dumps(value) + '; ' + days + ".dispatchEvent(new Event('input'))")
            self.assertTrue(self.js(window, find + '.disabled'))
            self.assertFalse(self.js(window, "document.querySelector('#cf-git-task-days-error').hidden"))
            self.js(window, find + '.click()')
            self.assertEqual(len(http.requests), before)
            self.assertIs(window.bridge.git_task_filter, previous)
            self.assertEqual(window.bridge.selected, selected)
        # The bridge also rejects date overflow before starting any HTTP worker.
        for invalid in ('50000', '1000000000'):
            window.bridge.searchGitTask('ERP-123', False, invalid)
            self.assertIsNone(window.bridge.task)
            self.assertEqual(len(http.requests), before)
            self.assertIs(window.bridge.git_task_filter, previous)
            self.wait(lambda: self.js(window, "document.querySelector('dialog[aria-labelledby=cf-dialog-title]').open") is True)
            self.js(window, "document.querySelector('dialog[aria-labelledby=cf-dialog-title]').close()")
        self.js(window, period + ".click(); " + menu + ".querySelector('[value=all]').click()")
        self.assertTrue(self.js(window, "document.querySelector('[data-git-task-days]').hidden"))
        self.assertTrue(self.js(window, days + '.disabled'))
        self.assertFalse(self.js(window, find + '.disabled'))

    def test_connection_open_index_preview_and_preserved_state_on_failure(self):
        from cf_wizard.qt_view import QtWizardWindow
        from cf_wizard.qt_git_dialog import GitConnectionDialog
        from cf_wizard.git_api import GitError, credential_target
        from cf_wizard.journals import ErrorJournal
        from cf_wizard.paths import PathPreferences
        from test_cf_wizard import DOCUMENT
        from test_cf_wizard_github import GitHubHTTP, client_for, TOKEN, REV
        case=self.case; wizard=case.indexed_wizard()
        bodies={'config/'+p.relative_to(case.export).as_posix():p.read_bytes() for p in case.export.rglob('*') if p.is_file()}
        http=GitHubHTTP(bodies); client=client_for(http)
        settings=self.settings(); settings.update(provider='github',server='https://github.com')
        paths=PathPreferences(values=dict(destination=str(case.output),logs=str(case.logs)),source_mode='git',git_settings=settings)
        window=QtWizardWindow(wizard,(),paths,lambda _:None,case.logs,ErrorJournal(case.logs))
        window.show(); bridge=window.bridge
        def dispose():
            from shiboken6 import delete, isValid
            if isValid(window):
                window.close()
                # Destroy the page while its profile and QApplication are still alive.
                delete(window.view.page())
                delete(window)
                self.app.processEvents()
        self.addCleanup(dispose)
        self.wait(lambda:self.js(window,"document.readyState")=='complete')
        dialog=GitConnectionDialog(window,settings,[],lambda _:None)
        dialog.secret.setText(TOKEN)
        with patch('cf_wizard.qt_view.GitConnectionDialog',return_value=dialog),patch.object(dialog,'exec',return_value=QDialog.DialogCode.Accepted),patch('cf_wizard.qt_view.create_client',return_value=client),patch('cf_wizard.qt_view.WindowsTokenStore') as vault:
            bridge.command('git-connect'); self.wait(lambda:bridge.task is None)
            self.assertIs(bridge.git_client,client); vault.return_value.load.assert_not_called(); vault.return_value.save.assert_not_called()
        bridge.command('open-source'); self.wait(lambda:bridge.task is None and bridge.index_check_task is None and bridge.catalog is not None)
        self.assertIn('GitHub',bridge.snapshot()['activeSource'])
        self.assertEqual(self.js(window,"document.querySelector('[data-source-mode=git]').textContent.trim()"),'Git')
        self.check_choice_menus(window, http)
        bridge.command('refresh-index'); self.wait(lambda:bridge.task is None and bridge.index_check_task is None)
        self.assertIsNotNone(bridge.index_file)
        bridge.markObject(DOCUMENT,True); bridge.command('preview'); self.wait(lambda:bridge.task is None)
        self.assertIsNotNone(bridge.preview)
        self.assertTrue(bridge.snapshot()['canPreview'])
        bridge._emit()
        self.wait(lambda: 'Предпросмотр готов' in self.js(window,"document.body.textContent"))
        if os.environ.get('CF_TEST_SCREENSHOT'):
            self.wait(lambda: self.js(window,"document.body.textContent.includes('Перенос целиком')"))
            window.grab().save(os.environ['CF_TEST_SCREENSHOT'])
        # A new SHA with unchanged metadata reuses SQLite, resets preview and
        # performs its read-only readiness check outside the Qt event thread.
        from test_cf_wizard_github import OLD
        saved = bridge.index_file.read_bytes()
        changed = dict(bodies)
        changed['config/Documents/Заказ/Ext/ObjectModule.bsl'] = b'// newer module'
        http.add_revision(OLD, changed, [REV])
        bridge.command('open-source')
        self.wait(lambda:bridge.task is None and bridge.index_check_task is None and bridge.catalog.git.revision == OLD)
        self.assertIsNone(bridge.preview)
        self.assertIsNotNone(bridge.index_file)
        self.assertEqual(bridge.index_file.read_bytes(), saved)
        self.assertIn('Индекс готов', bridge.index_info)
        calls = len(http.requests)
        bridge._sync_index()
        self.wait(lambda: bridge.index_check_task is None)
        self.assertEqual(len(http.requests), calls)
        # Metadata changes disable preview until the explicit update, while
        # the button still means Update (the existing database was retained).
        changed['config/Documents/Заказ.xml'] += b'\n<!-- new metadata -->'
        http.add_revision('e' * 40, changed, [OLD])
        bridge.command('open-source')
        self.wait(lambda:bridge.task is None and bridge.index_check_task is None and bridge.catalog.git.revision == 'e' * 40)
        self.assertIsNone(bridge.index_file)
        self.assertTrue(bridge.snapshot()['indexExists'])
        self.assertIn('Нужно обновить', bridge.index_info)
        bridge.command('refresh-index')
        self.wait(lambda:bridge.task is None and bridge.index_check_task is None)
        self.assertIsNotNone(bridge.index_file)
        bridge.markObject(DOCUMENT,True); bridge.command('preview'); self.wait(lambda:bridge.task is None)
        self.assertIsNotNone(bridge.preview)
        old_catalog, old_preview=bridge.catalog,bridge.preview
        # A failed connection must not invalidate the opened revision/selection/preview.
        failing=Mock(label='Gitea'); failing.branches.side_effect=GitError('Synthetic denied',401)
        dialog=GitConnectionDialog(window,settings,[],lambda _:None); dialog.secret.setText(TOKEN)
        with patch('cf_wizard.qt_view.GitConnectionDialog',return_value=dialog),patch.object(dialog,'exec',return_value=QDialog.DialogCode.Accepted),patch('cf_wizard.qt_view.create_client',return_value=failing),patch('cf_wizard.qt_view.WindowsTokenStore'):
            bridge.command('git-connect'); self.wait(lambda:bridge.task is None)
        self.assertIs(bridge.catalog,old_catalog); self.assertIs(bridge.preview,old_preview)
        self.assertEqual(bridge.selected,{DOCUMENT})
        with patch('cf_wizard.qt_view.WindowsTokenStore') as vault:
            other=dict(settings,provider='gitea',server='https://gitea.example.test')
            bridge._forget_git(other)
            self.assertIs(bridge.catalog,old_catalog); self.assertIs(bridge.git_client,client)
            vault.assert_called_with(credential_target(other['server'],'gitea'))
        self.assertNotIn(TOKEN,json.dumps(bridge.snapshot()))
        # Open the local source through the same command path and keep source-switch prompt in test control.
        bridge.setSourceMode('local'); bridge.values['export']=str(case.export)
        with patch.object(QMessageBox,'exec',return_value=QMessageBox.StandardButton.No):
            bridge.command('open-source'); self.wait(lambda:bridge.task is None and bridge.catalog is not None and bridge.catalog.git is None)
        self.assertEqual(bridge.catalog.configuration,case.source.configuration)
        dispose()


if __name__=='__main__':
    unittest.main()
