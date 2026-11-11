"""Native connection form and WebEngine flow, isolated from user credentials."""
import json
import os
from pathlib import Path
import time
import unittest
from unittest.mock import Mock, patch
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
        bridge.command('open-source'); self.wait(lambda:bridge.task is None and bridge.catalog is not None)
        self.assertIn('GitHub',bridge.snapshot()['activeSource'])
        self.assertEqual(self.js(window,"document.querySelector('[data-source-mode=git]').textContent.trim()"),'Git')
        bridge.command('refresh-index'); self.wait(lambda:bridge.task is None)
        self.assertIsNotNone(bridge.index_file)
        bridge.markObject(DOCUMENT,True); bridge.command('preview'); self.wait(lambda:bridge.task is None)
        self.assertIsNotNone(bridge.preview)
        self.assertTrue(bridge.snapshot()['canPreview'])
        bridge._emit()
        self.wait(lambda: 'Предпросмотр готов' in self.js(window,"document.body.textContent"))
        if os.environ.get('CF_TEST_SCREENSHOT'):
            self.wait(lambda: self.js(window,"document.body.textContent.includes('Перенос целиком')"))
            window.grab().save(os.environ['CF_TEST_SCREENSHOT'])
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