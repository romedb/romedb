#!/usr/bin/env python3
"""Native client against a synthetic import protocol, with process-loss faults."""
import hashlib
import http.server
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import uuid

binary=sys.argv[1]
key='rdbo_'+'b'*64
org=str(uuid.uuid4())
PART=8*1024*1024
jobs={}; requests=[]; parts=[]
mode='lost-create'
class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self,*_): pass
    def handle_request(self):
        global mode
        assert self.headers.get('authorization')=='Bearer '+key
        body=self.rfile.read(int(self.headers.get('content-length',0)))
        requests.append((self.command,self.path))
        path,_,query=self.path.partition('?')
        pieces=path.split('/')
        assert pieces[:4]==['','v1','organizations',org]
        assert pieces[4]=='imports'
        identity=pieces[5]
        job=jobs.get(identity)
        code=200
        if mode=='denied': code,result=403,{'error':'revoked '+key}
        elif mode=='redirect':
            self.send_response(307);self.send_header('Location','https://example.invalid/');self.send_header('Content-Length','0');self.end_headers();return
        elif self.command=='PUT' and len(pieces)==6:
            declaration=json.loads(body)
            if job:
                assert all(job[k]==v for k,v in declaration.items())
            else:
                job={'id':identity,'organizationId':org,**declaration,'state':'uploading','expiresAt':9999999999999,'partReceipts':[],'nextPart':None}
                jobs[identity]=job
            result=dict(job);code=202
            if mode=='lost-create': mode='normal';code,result=503,{'error':'lost create acknowledgment '+key}
        elif not job: code,result=404,{'error':'missing'}
        elif self.command=='GET':
            result=dict(job)
            after=int(query.split('=')[1]) if query else 0
            # One-entry pages deliberately exercise the cursor.
            receipts=[p for p in job['partReceipts'] if p['number']>after]
            result['partReceipts']=receipts[:1]
            result['nextPart']=receipts[0]['number'] if len(receipts)>1 else None
            if mode=='bad-receipt' and receipts: result['partReceipts']=[{**receipts[0],'sha256':'0'*64}]
            if mode=='bad-identity':result['id']=str(uuid.uuid4())
            if mode=='bad-cursor':result['nextPart']=0
        elif self.command=='PUT' and pieces[6]=='parts':
            n=int(pieces[7]);assert len(body)<=PART
            checksum=hashlib.sha256(body).hexdigest();assert self.headers['x-romedb-part-sha256']==checksum
            receipt={'number':n,'size':len(body),'sha256':checksum,'state':'uploaded'}
            assert not any(p['number']==n for p in job['partReceipts']), 'confirmed part retransmitted'
            job['partReceipts'].append(receipt);parts.append((identity,n,len(body)));result=receipt
            if mode=='lost-part':mode='normal';code,result=503,{'error':'lost part acknowledgment'}
            if mode=='changed-during-upload':
                with source.open('r+b') as f:f.seek(PART);f.write(b'!')
        elif pieces[6]=='complete':
            assert sum(p['size'] for p in job['partReceipts'])==job['size']
            job['state']='ready';job['result']={'databaseId':job['databaseId'],'bytes':8192,'logicalRows':2};result=job
            if mode=='lost-complete':mode='normal';code,result=503,{'error':'lost completion acknowledgment'}
        elif pieces[6]=='cancel':
            if job['state']!='ready':job['state']='canceled'
            result=job
        else:raise AssertionError(self.path)
        encoded=json.dumps(result).encode()
        self.send_response(code);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(encoded)));self.end_headers();self.wfile.write(encoded)
    do_GET=handle_request
    do_PUT=handle_request
    do_POST=handle_request
server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler)
threading.Thread(target=server.serve_forever,daemon=True).start()
env={**os.environ,'ROMEDB_API_KEY':key,'ROMEDB_CLOUD_URL':f'http://127.0.0.1:{server.server_port}'}
def run(*args,success=True,settings=None):
    result=subprocess.run([binary,'cloud','import',*map(str,args)],capture_output=True,text=True,env=settings or env,timeout=20)
    assert (result.returncode==0)==success,(args,result.returncode,result.stdout,result.stderr)
    assert key not in result.stdout+result.stderr
    return result
try:
    with tempfile.TemporaryDirectory() as tmp:
        tmp=Path(tmp);source=tmp/'snapshot.sqlite';journal=tmp/'journal.json'
        original=b'SQLite format 3\0'+bytes(PART+4096-16);source.write_bytes(original)
        run('start',org,'cli-import',source,journal,success=False)
        saved=json.loads(journal.read_text());identity=saved['id']
        assert len(jobs)==1 and identity in jobs and key not in journal.read_text()
        assert journal.stat().st_mode&0o077==0
        before=len(requests);run('start',org,'cli-import',source,journal,success=False);assert len(requests)==before
        mode='lost-part';run('resume',journal,source,success=False)
        assert parts==[(identity,1,PART)]
        source.write_bytes(original[:-1]+b'x')
        run('resume',journal,source,success=False);assert len(parts)==1
        source.write_bytes(original)
        for mode in ['bad-receipt','bad-identity','bad-cursor','denied','redirect']:
            run('resume',journal,source,success=False);assert len(parts)==1
        mode='lost-complete';run('resume',journal,source,success=False)
        assert parts==[(identity,1,PART),(identity,2,4096)]
        assert json.loads(run('resume',journal,tmp/'no-file-needed-after-ready').stdout)['state']=='ready'
        assert json.loads(run('status',org,identity).stdout)['result']['logicalRows']==2
        before=len(requests);run('cancel',org,identity,success=False);assert len(requests)==before
        assert json.loads(run('cancel',org,identity,'--confirm').stdout)['state']=='ready'
        # Journal is immutable, including after a completed import.
        assert json.loads(journal.read_text())==saved
        other={**env,'ROMEDB_CLOUD_URL':'http://127.0.0.1:1'}
        run('resume',journal,source,settings=other,success=False)
        # Re-open the retained job as uploading only in this synthetic fixture to
        # exercise paginated two-part receipt recovery without any retransmission.
        jobs[identity]['state']='uploading';mode='normal'
        assert json.loads(run('resume',journal,source).stdout)['state']=='ready';assert len(parts)==2
        source.write_bytes(b'invalid')
        before=len(requests);run('start',org,'invalid',source,tmp/'invalid.json',success=False)
        assert len(requests)==before and not (tmp/'invalid.json').exists()
        source.write_bytes(original)
        mode='changed-during-upload';run('start',org,'changed',source,tmp/'changed.json',success=False)
        changed=json.loads((tmp/'changed.json').read_text())['id']
        assert [p[1] for p in parts if p[0]==changed]==[1]
        mode='normal'
        assert json.loads(run('cancel',org,changed,'--confirm').stdout)['state']=='canceled'
        # An existing file (including the snapshot itself) is never overwritten.
        before=source.read_bytes();run('start',org,'overwrite',source,source,success=False);assert source.read_bytes()==before
        assert len(jobs)==2
        print('Import CLI: fixed journal, process restart, lost create/part/complete replies, bounded parts, receipt pagination, source mutation, origin binding, cancellation, redaction and redirects passed')
finally:server.shutdown();server.server_close()
