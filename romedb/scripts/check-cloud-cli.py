#!/usr/bin/env python3
"""Generic cloud HTTP client contract; only synthetic credentials and data."""
import http.server
import json
import os
import subprocess
import sys
import threading
import tempfile
from pathlib import Path
import uuid

binary = sys.argv[1]
key = 'fdbo_' + 'a' * 64
database = str(uuid.uuid4())
organization = str(uuid.uuid4())
base = f"/v1/organizations/{organization}/databases"
receipts = {}
sequence = 7
requests = []
mode = 'normal'
query_attempts = []
versions = []

class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def handle_request(self):
        assert self.headers.get('authorization') == f'Bearer {key}'
        body = self.rfile.read(int(self.headers.get('content-length', 0)))
        value = json.loads(body) if body else None
        requests.append((self.command, self.path, value))
        if self.path.endswith(('/read', '/query')):
            versions.append(self.headers.get('x-romedb-query-version'))
        status = 200
        result = {'id': database, 'sequence': sequence, 'url': 'https://untrusted.example/query'}
        if self.path not in ['/v1/whoami', '/v1/organizations'] and not self.path.startswith(base):
            status, result = 403, {'error': 'wrong organization'}
        elif mode == 'large':
            result = {'data': 'x' * (600 * 1024)}
        elif mode == 'redirect':
            status = 307
            result = {'error': f'echo {key}'}
        elif self.path == '/v1/whoami':
            result = {'organizationId': organization, 'scopes': ['read', 'query', 'manage'], 'echo': key}
        elif self.path == '/v1/organizations':
            result = {'organizations': [{'id': organization}]}
        elif self.path.endswith('/read'):
            assert value['readVersion'] == 2
            if mode == 'read-failed':
                status, result = 503, {'error': 'read unavailable'}
            else:
                result = {'readVersion': 2, 'requestId': value['requestId'], 'sequence': sequence, 'results': []}
        elif self.path.endswith('/query'):
            query_attempts.append(value)
            if mode == 'retry' and len(query_attempts) == 1:
                status, result = 503, {'error': 'not acknowledged'}
            elif mode == 'unresolved':
                status, result = (503 if len(query_attempts) == 1 else 409), {'error': 'unknown outcome'}
            else:
                result = {'requestId': value['requestId'], 'sequence': value['afterSequence']+(3 if mode == 'intervening' else 1), 'results': []}
        if self.path.endswith(('/read', '/query')) and mode in ['lost', 'replay', 'wrong-reply']:
            if self.path.endswith('/query'):
                saved = receipts.get(value['requestId'])
                if saved:
                    assert saved[0] == value, 'request identity changed on replay'
                    result = saved[1]
                else:
                    receipts[value['requestId']] = (value, result)
            if mode == 'lost': status, result = 503, {'error': 'lost committed response '+key}
            if mode == 'wrong-reply': result = {**result, 'requestId': str(uuid.uuid4())}
        self.send_response(status)
        if status == 307:
            self.send_header('Location', 'https://untrusted.example/')
        self.send_header('Content-Type', 'application/json')
        encoded = json.dumps(result).encode()
        self.send_header('Content-Length', str(len(encoded)))
        self.end_headers()
        self.wfile.write(encoded)

    do_GET = handle_request
    do_POST = handle_request
    do_DELETE = handle_request

server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()
env = {**os.environ, 'ROMEDB_API_KEY': key, 'ROMEDB_CLOUD_URL': f'http://127.0.0.1:{server.server_port}', 'ROMEDB_ORGANIZATION_ID': organization}
def run(args, sql='', success=True, settings=None):
    result = subprocess.run([binary, 'cloud', *args], input=sql, text=True, capture_output=True,
                            env=settings or env, timeout=20)
    assert (result.returncode == 0) == success, (args, result.returncode, result.stderr)
    assert key not in result.stdout + result.stderr, 'credential printed in diagnostics/output'
    return result
try:
    assert 'whoami' in run(['--help']).stdout
    assert '[redacted]' in run(['whoami']).stdout
    for args, method, path in [
        (['db', 'list'], 'GET', base),
        (['db', 'create', 'example'], 'POST', base),
        (['db', 'show', database], 'GET', f'{base}/{database}'),
        (['db', 'delete', database], 'DELETE', f'{base}/{database}'),
        (['db', 'warm', database], 'POST', f'{base}/{database}/warm'),
    ]:
        run(args)
        assert requests[-1][:2] == (method, path)
    assert requests[-1][2] == {}
    before = len(requests)
    run(['db', 'access', database], "SELECT 1;\n.quit\n")
    assert not any(path.endswith('/warm') for _, path, _ in requests[before:]), 'piped access sends no warm request'
    details_gets = lambda: sum(1 for request in requests if request[:2] == ('GET', f'{base}/{database}'))
    before = details_gets()
    run(['db', 'access', database], "SELECT 1;\n.quit\n")
    one_statement_gets = details_gets() - before
    query_attempts.clear(); before = details_gets()
    run(['db', 'access', database], "SELECT 1;\nSELECT\n2;\n.quit\n")
    assert len(query_attempts) == 2
    assert [q['afterSequence'] for q in query_attempts] == [7, 8], 'a sole confirmed commit supplies the next sequence'
    assert details_gets() - before == one_statement_gets, 'the second statement needs no details request'
    assert len({q['requestId'] for q in query_attempts}) == 2
    run(['db', 'access', database], 'SELECT 1; SELECT 2;\n.quit\n')
    assert len(query_attempts[-1]['statements']) == 2
    with tempfile.TemporaryDirectory() as directory:
        directory=Path(directory)
        journal=directory/'read.json'
        before=len(requests)
        result=run(['db','read',database,str(journal)],'SELECT 1; SELECT 2;')
        assert len(requests)==before+2, 'tracked read gets sequence then submits once'
        method, path, body=requests[-1]
        assert method=='POST' and path==f'{base}/{database}/read'
        assert set(body)=={'statements','requestId','afterSequence','readVersion'} and len(body['statements'])==2
        assert json.loads(result.stdout)['sequence']==7 and json.loads(result.stdout)['readVersion']==2
        assert journal.stat().st_mode&0o077==0 and key not in journal.read_text()
        assert json.loads(journal.read_text())['request']==body
        assert json.loads(journal.read_text())['version']==5 and versions[-1]=='3', 'new read journals use query version 3'
        before=len(requests);run(['db','read',database,str(journal)],'SELECT 99;',success=False)
        assert len(requests)==before, 'existing journal must never be overwritten or submit new SQL'
        for sql in ['', 'x'*(1024*1024+1), 'SELECT 1;'*33]:
            before=len(requests);run(['db','read',database,str(directory/'invalid.json')],sql,success=False)
            assert len(requests)==before
        for operation in ['read','query']:
            journal=directory/(operation+'-lost.json');mode='lost'
            result=run(['db',operation,database,str(journal)],'SELECT 2;',success=False)
            original=json.loads(journal.read_text());assert json.loads(result.stdout)['outcome']=='unresolved'
            sequence=50;mode='replay';before=len(requests)
            result=run(['db','retry',str(journal)])
            assert len(requests)==before+1 and requests[-1][2]==original['request']
            assert json.loads(result.stdout)['sequence']==(50 if operation=='read' else original['request']['afterSequence']+1)
            assert json.loads(journal.read_text())==original
            before=len(requests)
            run(['db','retry',str(journal)],success=False,settings={**env,'ROMEDB_ORGANIZATION_ID':str(uuid.uuid4())})
            run(['db','retry',str(journal)],success=False,settings={**env,'ROMEDB_CLOUD_URL':'http://127.0.0.1:1'})
            assert len(requests)==before
            mode='wrong-reply';run(['db','retry',str(journal)],success=False)
        mode='read-failed';before=len(requests)
        run(['db','read',database,str(directory/'failed.json')],'SELECT 1;',success=False)
        assert len(requests)==before+2, 'failed read must not retry automatically'
        legacy=json.loads((directory/'read.json').read_text());legacy['version']=1
        legacy['request']['expectedSequence']=legacy['request'].pop('afterSequence')
        (directory/'legacy.json').write_text(json.dumps(legacy));before=len(requests)
        run(['db','retry',str(directory/'legacy.json')],success=False)
        assert len(requests)==before, 'old journals must be reconciled before cutover, not translated'
        legacy_read=json.loads((directory/'read.json').read_text());legacy_read['version']=2
        legacy_read['request'].pop('readVersion')
        (directory/'old-read.json').write_text(json.dumps(legacy_read));before=len(requests)
        run(['db','retry',str(directory/'old-read.json')],success=False)
        assert len(requests)==before, 'old read replay journals must not silently rerun'
        current=json.loads((directory/'read.json').read_text());current['version']=3
        (directory/'v280-read.json').write_text(json.dumps(current));os.chmod(directory/'v280-read.json',0o600);before=len(requests)
        previous_mode=mode;mode='normal';run(['db','retry',str(directory/'v280-read.json')]);mode=previous_mode
        assert len(requests)==before+1 and versions[-1] is None, 'a 2.8.0 read journal replays at query version 2'
        (directory/'huge.json').write_text('x'*(2*1024*1024+5000));before=len(requests)
        run(['db','retry',str(directory/'huge.json')],success=False);assert len(requests)==before
    mode='intervening'
    run(['db','access',database], 'SELECT 1;\n.quit\n')
    query_attempts.clear(); before = details_gets()
    run(['db','access',database], 'SELECT 1;\nSELECT 2;\n.quit\n')
    assert [q['afterSequence'] for q in query_attempts] == [sequence, sequence], 'another writer between statements forces a fresh sequence'
    assert details_gets() - before == one_statement_gets + 1
    mode='normal'
    sequence=7
    query_attempts.clear()
    mode = 'retry'
    result = run(['db', 'access', database], 'INSERT INTO t VALUES (1);\n.retry\n.quit\n', success=False)
    assert len(query_attempts) == 2 and query_attempts[0] == query_attempts[1]
    assert json.loads(result.stdout.splitlines()[-1])['sequence'] == 8
    query_attempts.clear()
    mode = 'unresolved'
    run(['db', 'access', database], 'INSERT INTO t VALUES (2);\n.retry\nINSERT INTO t VALUES (3);\n.quit\n', success=False)
    assert len(query_attempts) == 2 and query_attempts[0] == query_attempts[1]
    query_attempts.clear();mode='wrong-reply'
    run(['db','access',database],'SELECT 1;\n.retry\nSELECT 2;\n.quit\n',success=False)
    assert len(query_attempts)==2 and query_attempts[0]==query_attempts[1], 'unconfirmed replies cannot clear interactive pending work'
    mode = 'redirect'
    before = len(requests)
    run(['whoami'], success=False)
    assert len(requests) == before + 1
    mode = 'large'
    run(['whoami'], success=False)
    for endpoint in ['http://example.test', 'https://user:pass@example.test', 'https://example.test/?key=bad', 'romedb://user:pass@example.test', 'romedb://example.test/?key=bad', 'romedb:example.test']:
        run(['whoami'], success=False, settings={**env, 'ROMEDB_CLOUD_URL': endpoint})
    mode='normal'
    before=len(requests)
    run(['db','list'],success=False,settings={k:v for k,v in env.items() if k!='ROMEDB_ORGANIZATION_ID'})
    run(['db','list'],success=False,settings={**env,'ROMEDB_ORGANIZATION_ID':'../metadata'})
    run(['db', 'show', '../metadata'], success=False)
    assert len(requests)==before
    run(['--organization',organization,'db','show',database],settings={**env,'ROMEDB_ORGANIZATION_ID':str(uuid.uuid4())})
    assert requests[-1][1]==f'{base}/{database}'
    assert json.loads(run(['organizations']).stdout)['organizations'][0]['id']==organization
    issuance = str(uuid.uuid4())
    expiry = 1900000000000
    run(['db','token-create',database,issuance,'service','write',str(expiry)])
    assert requests[-1] == ('POST',f'{base}/{database}/tokens',{'id':issuance,'name':'service','scopes':['read','query'],'expiresAt':expiry})
    run(['db','tokens',database])
    assert requests[-1][:2] == ('GET',f'{base}/{database}/tokens')
    run(['db','token-revoke',database,issuance])
    assert requests[-1][:2] == ('DELETE',f'{base}/{database}/tokens/{issuance}')
    key = 'eyJhbGciOiJFUzI1NiJ9.eyJzdWIiOiJmaXh0dXJlIn0.c2lnbmF0dXJl'
    env['ROMEDB_API_KEY'] = key
    run(['db','show',database])
    run(['db','access',database],'SELECT 1;\n.quit\n')
    before = len(requests)
    for invalid in ['a.b', 'a..b', 'a.b.c.d', 'a.b.c\n', 'a.b.'+'x'*4096]:
        run(['db','show',database],success=False,settings={**env,'ROMEDB_API_KEY':invalid})
    assert len(requests) == before
    print('Cloud CLI management, interactive batching, stable retries, redaction and endpoint checks passed')
finally:
    server.shutdown()
    server.server_close()
