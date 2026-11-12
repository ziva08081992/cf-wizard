"""Provider isolation and complete GitHub reads, using synthetic HTTP only."""
from concurrent.futures import CancelledError
from hashlib import sha1, sha256
from io import BytesIO
import json
from pathlib import Path
from tempfile import TemporaryDirectory
from threading import Event
import unittest
from unittest.mock import Mock, patch
from urllib.error import HTTPError
from urllib.parse import urlsplit, parse_qs, unquote

from cf_wizard.git_api import Blob, Tree, GitError, create_client, credential_target, _NoRedirect
from cf_wizard.git_source import GitFiles, ExportSourceFactory
from cf_wizard.github import GitHubClient, BLOB_LIMIT
from cf_wizard.gitea import GiteaClient
from cf_wizard.paths import PathPreferences, PathSettingsFile

REV = 'a' * 40
OLD = 'b' * 40
TOKEN = 'synthetic-provider-token'


def oid(text):
    return sha1(text.encode()).hexdigest()


def blob(data):
    return Blob(sha1(f'blob {len(data)}\0'.encode() + data).hexdigest(), len(data))


class GitHubHTTP:
    """Minimal HTTP fixture: production parsers, transport and hash checks stay active."""
    def __init__(self, bodies, revision=REV):
        self.requests, self.blobs, self.trees, self.commits = [], {}, {}, {}
        self.graphql_count = self.raw_count = 0
        self.history = []
        self.graphql_edit = lambda data: data
        self.add_revision(revision, bodies)

    def add_revision(self, revision, bodies, parents=()):
        def tree(prefix):
            entries = {}
            for path, data in bodies.items():
                if not path.startswith(prefix):
                    continue
                name, sep, _ = path[len(prefix):].partition('/')
                if sep:
                    child = tree(prefix + name + '/')
                    entries[name] = dict(path=name, type='tree', mode='040000', sha=child)
                else:
                    entry = blob(data)
                    self.blobs[entry.oid] = data
                    entries[name] = dict(path=name, type='blob', mode='100644', sha=entry.oid, size=len(data))
            rows = sorted(entries.values(), key=lambda r:r['path'])
            key = oid(json.dumps(rows, sort_keys=True))
            self.trees[key] = dict(sha=key, truncated=False, tree=rows)
            return key
        root = tree('')
        self.commits[revision] = dict(sha=revision, tree=dict(sha=root), parents=[dict(sha=p) for p in parents])
        self.head = revision

    def open(self, request, timeout):
        assert timeout == 20
        assert urlsplit(request.full_url).netloc == 'api.github.com'
        assert request.get_header('Authorization') == 'Bearer ' + TOKEN
        self.requests.append(request)
        url = urlsplit(request.full_url)
        path = unquote(url.path)
        if path == '/graphql':
            assert request.method == 'POST'
            self.graphql_count += 1
            payload = json.loads(request.data)
            assert 'mutation' not in payload['query']
            rows = {}
            for key, value in payload['variables'].items():
                if key.startswith('oid'):
                    data = self.blobs[value]
                    try:
                        text, binary = data.decode('utf8'), False
                    except UnicodeError:
                        text, binary = None, True
                    rows['b'+key[3:]] = dict(__typename='Blob', oid=value, byteSize=len(data),
                                             isBinary=binary, isTruncated=False, text=text)
            data = dict(data=dict(repository=rows, rateLimit=dict(cost=1, remaining=4999)))
            return BytesIO(json.dumps(self.graphql_edit(data)).encode())
        suffix = path.removeprefix('/repos/example/metadata/')
        if suffix == 'commits':
            value = self.history if parse_qs(url.query)['page'] == ['1'] else []
        elif suffix == 'branches':
            value = [dict(name='feature/Тест')] if parse_qs(url.query)['page'] == ['1'] else []
        elif suffix.startswith('git/ref/heads/'):
            value = dict(ref='refs/heads/'+suffix.removeprefix('git/ref/heads/'), object=dict(type='commit', sha=self.head))
        elif suffix.startswith('git/commits/'):
            value = self.commits[suffix.removeprefix('git/commits/')]
        elif suffix.startswith('git/trees/'):
            assert not url.query  # No recursive=false or pagination borrowed from Gitea.
            value = self.trees[suffix.removeprefix('git/trees/')]
        elif suffix.startswith('git/blobs/'):
            self.raw_count += 1
            assert request.get_header('Accept') == 'application/vnd.github.raw+json'
            return BytesIO(self.blobs[suffix.removeprefix('git/blobs/')])
        else:
            raise AssertionError('Unexpected synthetic endpoint: '+suffix)
        return BytesIO(json.dumps(value).encode())


def client_for(fixture):
    client = GitHubClient('https://github.com', 'example/metadata', TOKEN)
    client._opener = fixture
    return client


class ProviderTests(unittest.TestCase):
    def test_factory_and_legacy_settings_keep_gitea_identity(self):
        server = 'https://git.example.test'
        old = 'CF-Wizard/Gitea/' + sha256(server.encode()).hexdigest()[:24]
        self.assertEqual(credential_target(server), old)
        self.assertIsInstance(create_client(dict(server=server, repository='example/metadata'), TOKEN), GiteaClient)
        self.assertIsInstance(create_client(dict(provider='github', server='https://api.github.com', repository='example/metadata'), TOKEN), GitHubClient)
        self.assertEqual(credential_target('https://github.com', 'github'), credential_target('https://api.github.com', 'github'))
        self.assertNotEqual(credential_target('https://github.com'), credential_target('https://github.com', 'github'))
        with self.assertRaises(ValueError):
            create_client(dict(provider='invalid', server=server, repository='example/metadata'), TOKEN)
        with self.assertRaises(ValueError):
            create_client(dict(provider='github', server=server, repository='example/metadata'), TOKEN)

    def test_gitea_get_batch_limit_stays_provider_specific(self):
        from base64 import b64encode
        from urllib.parse import urlencode
        client=GiteaClient('https://gitea.example.test','example/metadata',TOKEN)
        paths=['config/Catalogs/'+('Д'*150)+str(i)+'.xml' for i in range(20)]
        seen=[]
        def receive(endpoint,params,operation):
            self.assertEqual(endpoint,'file-contents')
            self.assertLessEqual(len(urlencode(params)),4500)
            batch=json.loads(params['body'])['files']; seen.extend(batch)
            return [dict(path=p,encoding='base64',content=b64encode(b'x').decode()) for p in batch]
        with patch.object(client,'_json',side_effect=receive) as request:
            self.assertEqual(client.files(REV,paths),dict.fromkeys(paths,b'x'))
        self.assertEqual(seen,paths); self.assertGreater(request.call_count,1)
        # The shared metadata loader must not impose the Gitea GET URL limit on GitHub.
        source=object.__new__(GitFiles); source.directory='config'; source.revision=REV
        source._entries=lambda _:{}; source.blob=lambda _:blob(b'x'); source._cached=lambda _:None
        source.ensure=lambda _:None; source.client=Mock()
        source.client.files.return_value={}
        source.metadata({p:p.removeprefix('config/') for p in paths})
        self.assertEqual(source.client.files.call_count,1)

    def test_settings_roundtrip_and_provider_cache_separation(self):
        with TemporaryDirectory() as temp:
            store = PathSettingsFile(Path(temp)/'paths.json')
            store.path.write_text(json.dumps(dict(git_settings=dict(server='https://git.test', branch='old'))))
            self.assertNotIn('provider', store.load().git_settings)
            store.save(PathPreferences(git_settings=dict(provider='github', server='https://github.com', repository='example/metadata')))
            self.assertEqual(store.load().git_settings['provider'], 'github')
            self.assertNotIn(TOKEN, store.path.read_text())
            gh = GitHubClient('https://github.com','example/metadata',TOKEN)
            gt = GiteaClient('https://github.com','example/metadata',TOKEN)
            first = GitFiles(gh,'config','main',REV,{},Path(temp))
            second = GitFiles(gt,'config','main',REV,{},Path(temp))
            self.assertNotEqual(first.preference_key,second.preference_key)
            self.assertNotEqual(first.root,second.root)

    def test_secrets_errors_redirects_and_fixed_origin(self):
        client = GitHubClient('https://github.com','example/metadata',TOKEN)
        for code in (301,401,403,429):
            error = HTTPError('https://api.github.com',code,TOKEN,{'Retry-After':'30'},BytesIO(TOKEN.encode()))
            with self.subTest(code=code), patch.object(client._opener,'open',side_effect=error) as send:
                with self.assertRaises(GitError) as caught:
                    client._get('branches',{},'Чтение')
                self.assertEqual(caught.exception.status,code)
                self.assertNotIn(TOKEN,str(caught.exception))
                self.assertEqual(send.call_count,1)
                req=send.call_args.args[0]
                self.assertEqual(req.get_header('Authorization'),'Bearer '+TOKEN)
                self.assertEqual(req.get_header('X-github-api-version'),'2026-03-10')
        with patch.object(client._opener,'open') as send, self.assertRaises(GitError):
            client._request('https://other.example/','Не отправлять')
        send.assert_not_called()
        self.assertIsNone(_NoRedirect().redirect_request(None,None,302,'',{},'https://other.example'))


class GitHubReads(unittest.TestCase):
    def setUp(self):
        self.bodies = {'config/Configuration.xml':b'<root/>', 'config/Catalogs/A.xml':'\ufeff<xml>Кириллица\r\n</xml>'.encode(),
                       'config/Catalogs/A/Ext/ObjectModule.bsl':b'large-module', 'config/Other.bin':b'\xff\x00'}
        self.http=GitHubHTTP(self.bodies)
        self.client=client_for(self.http)

    def test_branches_refs_pinned_tree_and_raw_files(self):
        self.assertEqual(self.client.branches(),['feature/Тест'])
        self.assertEqual(self.client.branch_sha('feature/Тест'),REV)
        tree=self.client.config_tree(REV,'config')
        self.assertIsInstance(tree['Catalogs'],Tree)
        self.http.head=OLD
        self.assertEqual(self.client.file(REV,'config/Configuration.xml',tree['Configuration.xml']),b'<root/>')
        ref=[r for r in self.http.requests if '/git/ref/' in r.full_url][0]
        self.assertIn('feature/%D0%A2',ref.full_url)

    def test_batches_preserve_bytes_and_use_binary_fallback(self):
        paths=['config/Catalogs/A.xml','config/Other.bin']
        self.assertEqual(self.client.files(REV,paths),{p:self.bodies[p] for p in paths})
        self.assertEqual(self.http.graphql_count,1)
        self.assertEqual(self.http.raw_count,1)
        self.assertEqual(self.client.rate_limit['cost'],1)

    def test_text_mismatch_and_truncation_fall_back_to_exact_blob(self):
        for edit in ('newline','truncated'):
            def change(data):
                row=data['data']['repository']['b0']
                if edit=='newline': row['text']=row['text'].replace('\r\n','\n')
                else: row['isTruncated']=True
                return data
            self.http.graphql_edit=change
            path='config/Catalogs/A.xml'
            self.assertEqual(self.client.files(REV,[path])[path],self.bodies[path])
        self.assertEqual(self.http.raw_count,2)

    def test_partial_graphql_errors_never_return_partial_success_or_secret(self):
        def missing(data):
            del data['data']['repository']['b0']
            return data
        def errors(data):
            data['errors']=[dict(message=TOKEN)]
            return data
        for edit in (missing,errors,lambda data:dict(data=dict(repository=None))):
            self.http.graphql_edit=edit
            with self.subTest(edit=edit),self.assertRaises(GitError) as caught:
                self.client.files(REV,['config/Catalogs/A.xml'])
            self.assertNotIn(TOKEN,str(caught.exception))
        self.assertEqual(self.http.raw_count,0)

    def test_batch_count_and_size_are_bounded(self):
        bodies={f'config/Catalogs/O{i}.xml':b'<root/>' for i in range(45)}
        self.http=GitHubHTTP(bodies); self.client=client_for(self.http)
        self.assertEqual(len(self.client.files(REV,list(bodies))),45)
        self.assertEqual(self.http.graphql_count,3)
        with patch('cf_wizard.github.BATCH_BYTES',10):
            self.client.files(REV,list(bodies)[:3])
        self.assertEqual(self.http.graphql_count,6)
        self.assertEqual(self.http.raw_count,0)

    def test_corrupt_raw_and_unsupported_large_blob(self):
        entry=blob(b'expected')
        with patch.object(self.client,'_get',return_value=b'wrong'), self.assertRaises(GitError):
            self.client.file(REV,'config/A.xml',entry)
        with patch.object(self.client,'_get') as send,self.assertRaises(GitError):
            self.client.file(REV,'config/A.xml',Blob(entry.oid,BLOB_LIMIT+1))
        send.assert_not_called()

    def test_unrelated_git_names_do_not_block_a_windows_export(self):
        bodies={**self.bodies,'outside:notes':b'irrelevant','CON':b'irrelevant'}
        self.http=GitHubHTTP(bodies); self.client=client_for(self.http)
        self.assertIn('Configuration.xml',self.client.config_tree(REV,'config'))
        self.http.add_revision(OLD,{'config/bad:notes.xml':b'invalid for Windows'})
        with TemporaryDirectory() as temp,self.assertRaises(ValueError):
            GitFiles(self.client,'config','main',OLD,self.client.config_tree(OLD,'config'),Path(temp))

    def test_malformed_and_truncated_trees_and_refs(self):
        for data in ([],{},dict(sha=REV,tree=[],truncated=True)):
            with patch.object(self.client,'_json',return_value=data):
                with self.assertRaises(GitError): self.client.tree(REV)
                with self.assertRaises(GitError): self.client.branch_sha('main')

    def test_case_collision_and_lfs_are_rejected(self):
        with TemporaryDirectory() as temp:
            with self.assertRaises(GitError):
                GitFiles(self.client,'config','main',REV,{'A.xml':blob(b''),'a.xml':blob(b'')},Path(temp))
            data=b'version https://git-lfs.github.com/spec/v1\n'
            source=GitFiles(self.client,'config','main',REV,{'A.xml':blob(data)},Path(temp))
            with self.assertRaises(GitError): source._put('A.xml',data)

    def test_custom_days_reach_github_http_with_pinned_revision(self):
        from datetime import timedelta
        from cf_wizard.application import GitTaskQuery
        query = GitTaskQuery.create('ERP-123', False, 14)
        result = list(self.client.commits(REV, since=query.since, until=query.until,
                                         cancelled=Event(), report=lambda _: None))
        self.assertEqual(result, [])
        params = parse_qs(urlsplit(self.http.requests[-1].full_url).query)
        self.assertEqual((params['sha'], params['since'], params['until']),
                         ([REV], [query.since.isoformat()], [query.until.isoformat()]))
        self.assertEqual(query.until - query.since, timedelta(days=14))

    def test_history_pagination_validation_and_cancellation(self):
        stop=Event(); report=[]
        rows=[[dict(sha=REV,commit=dict(message='ERP-123'))],[dict(sha=OLD,commit=dict(message='ERP-1234'))],[]]
        with patch.object(self.client,'_json',side_effect=rows) as request:
            result=list(self.client.commits(REV,since=None,until=None,cancelled=stop,report=report.append))
        self.assertEqual(result,[(REV,'ERP-123'),(OLD,'ERP-1234')])
        self.assertEqual(report,[1,2]); self.assertEqual(request.call_count,3)
        self.assertTrue(all(c.args[1]['sha']==REV for c in request.call_args_list))
        with patch.object(self.client,'_json',side_effect=[rows[0],rows[0]]),self.assertRaises(GitError):
            list(self.client.commits(REV,since=None,until=None,cancelled=stop,report=lambda _:None))
        stop.set()
        with patch.object(self.client,'_json') as send,self.assertRaises(CancelledError):
            list(self.client.commits(REV,since=None,until=None,cancelled=stop,report=lambda _:None))
        send.assert_not_called()

    def test_complete_paths_initial_merge_rename_delete_outside_config(self):
        self.http=GitHubHTTP({'config/A.xml':b'old','config/Deleted.xml':b'old','docs/a':b'old'},OLD)
        self.http.add_revision(REV,{'config/A.xml':b'new','config/Renamed.xml':b'old','docs/a':b'new'},[OLD,'c'*40])
        self.client=client_for(self.http)
        self.assertEqual(self.client.commit_paths(REV,directory='config'),['config/A.xml','config/Deleted.xml','config/Renamed.xml'])
        self.assertEqual(self.client.commit_paths(OLD,directory='config'),['config/A.xml','config/Deleted.xml'])
        self.assertFalse(any('git/commits/'+('c'*40) in r.full_url for r in self.http.requests))
        self.assertEqual(self.client.commit_paths(REV,directory='missing'),[])

    def test_mode_only_changes_are_included_in_commit_paths(self):
        self.http=GitHubHTTP({'config/A.xml':b'same'},OLD)
        self.http.add_revision(REV,{'config/A.xml':b'same'},[OLD])
        old_root=self.http.commits[OLD]['tree']['sha']
        config=self.http.trees[old_root]['tree'][0]['sha']
        changed=oid('mode changed')
        row=dict(self.http.trees[config]['tree'][0],mode='100755')
        self.http.trees[changed]=dict(sha=changed,truncated=False,tree=[row])
        new_root=oid('root mode changed')
        self.http.trees[new_root]=dict(sha=new_root,truncated=False,tree=[dict(path='config',type='tree',mode='040000',sha=changed)])
        self.http.commits[REV]['tree']['sha']=new_root
        self.client=client_for(self.http)
        self.assertEqual(self.client.commit_paths(REV,directory='config'),['config/A.xml'])

    def test_large_commits_are_complete_and_equal_subtrees_skipped(self):
        for count in (0,1,300,301,3000,3101):
            oldtree,newtree,subtree=oid('old'),oid('new'),oid('unchanged')
            before={'unchanged':Tree(subtree)}
            after={**before,**{f'O{i}.xml':blob(b'x') for i in range(count)}}
            with patch.object(self.client,'_commit',side_effect=lambda sha:(newtree,(OLD,)) if sha==REV else (oldtree,())), patch.object(self.client,'tree',side_effect=lambda sha:before if sha==oldtree else after) as trees:
                self.assertEqual(len(self.client.commit_paths(REV)),count)
                self.assertEqual(trees.call_count,2)

    def test_cancel_inside_tree_diff_stops_before_next_http(self):
        self.http=GitHubHTTP({'config/A.xml':b'old'},OLD)
        self.http.add_revision(REV,{'config/A.xml':b'new'},[OLD])
        self.client=client_for(self.http)
        stop=Event(); read=self.client.tree
        def cancel_after_tree(sha):
            result=read(sha); stop.set(); return result
        with patch.object(self.client,'tree',side_effect=cancel_after_tree) as trees,self.assertRaises(CancelledError):
            self.client.commit_paths(REV,cancelled=stop)
        self.assertEqual(trees.call_count,1)


class GitHubIndexIntegration(unittest.TestCase):
    def setUp(self):
        from test_cf_wizard import WizardTests
        self.case=WizardTests(); self.case.setUp(); self.addCleanup(self.case.doCleanups)

    def test_cold_warm_incremental_dependencies_and_failure_atomicity(self):
        from test_cf_wizard import DOCUMENT
        case=self.case
        bodies={'config/'+p.relative_to(case.export).as_posix():p.read_bytes() for p in case.export.rglob('*') if p.is_file()}
        bodies['config/Documents/Заказ/Ext/ObjectModule.bsl']=b'not needed for index'
        http=GitHubHTTP(bodies); client=client_for(http)
        wizard=case.indexed_wizard(); factory=ExportSourceFactory(wizard.open_export,case.root/'Git')
        source=factory.git(client,'main','config')
        self.assertEqual(http.raw_count,1)
        local=wizard.refresh_index(case.source,'').source
        update=wizard.refresh_index(source,''); source=update.source
        self.assertEqual(http.graphql_count,1); self.assertEqual(http.raw_count,1)
        first=wizard.preview(local,'local',{DOCUMENT},str(case.output))
        second=wizard.preview(source,'git',{DOCUMENT},str(case.output))
        self.addCleanup(wizard.discard,first); self.addCleanup(wizard.discard,second)
        self.assertEqual(first.composition,second.composition)
        self.assertIn('GitHub',update.log.read_text(encoding='utf8'))
        raw_before_incremental=http.raw_count
        calls=len(http.requests); warm=wizard.refresh_index(source,'')
        self.assertEqual(len(http.requests),calls); self.assertGreater(warm.unchanged,0)
        changed=dict(bodies); changed['config/Documents/Заказ.xml']+=b'\n'
        http.add_revision(OLD,changed,[REV]); next_source=factory.git(client,'main','config')
        new=wizard.refresh_index(next_source,'')
        self.assertEqual(new.changed,1); self.assertGreater(new.unchanged,0)
        self.assertEqual(http.graphql_count,2); self.assertEqual(http.raw_count,raw_before_incremental)
        from cf_wizard.application import GitTaskQuery
        http.history=[dict(sha=OLD,commit=dict(message='ERP-123 исправление')),dict(sha=REV,commit=dict(message='ERP-1234 другая задача'))]
        found=wizard.find_git_task(next_source,GitTaskQuery.create('ERP-123',True),Event(),lambda _:None)
        self.assertEqual(found.objects,frozenset({DOCUMENT})); self.assertEqual(found.commits,1)
        saved=new.path.read_bytes()
        with patch.object(client,'files',side_effect=GitError('synthetic failure')):
            # Force an XML cache miss without modifying the tested index.
            path=next_source.root/'Documents/Заказ.xml'; path.unlink()
            (next_source.git.blobs/blob(changed['config/Documents/Заказ.xml']).oid).unlink()
            with self.assertRaises(ValueError): wizard.refresh_index(next_source,'')
        self.assertEqual(new.path.read_bytes(),saved)


if __name__=='__main__':
    unittest.main()