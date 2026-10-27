import json, threading
from romedb import Database, CancellationToken, RomeDBError
heavy_read = 'SELECT count(*) FROM nums a,nums b,nums c,nums d,nums e,nums f,nums g,nums h,nums i'
heavy_write = 'INSERT INTO sink SELECT a.n FROM nums a,nums b,nums c,nums d,nums e,nums f,nums g,nums h,nums i'
results=[]
for start,end in [('BEGIN','COMMIT'),('SAVEPOINT caller','RELEASE caller')]:
    for scenario, query in [('control',None),('read_only',heavy_read),('write_only',heavy_write)]:
        with Database() as db:
            for sql in ['CREATE TABLE nums(n INTEGER)','CREATE TABLE sink(n INTEGER)','CREATE TABLE prior(n INTEGER)','INSERT INTO nums VALUES(0),(1),(2),(3),(4),(5),(6),(7),(8),(9)',start,'INSERT INTO prior VALUES(99)']:
                db.execute(sql)
            entry={'outer':start,'scenario':scenario}
            if query:
                token=CancellationToken()
                timer=threading.Timer(0.03,token.cancel)
                timer.start()
                try:
                    db.execute(query,token=token)
                    raise AssertionError('heavy query must cancel')
                except RomeDBError as err:
                    entry['cancel']={'code':err.code,'transaction':err.transaction,'message':str(err)}
                    assert err.code=='FDB_CANCELLED'
                finally: timer.join()
            entry['before_commit']={'sink':db.all('SELECT count(*) FROM sink'),'prior':db.all('SELECT * FROM prior'),'state':db.transaction_state}
            try:
                db.execute(end)
                entry['commit']='ok'
            except RomeDBError as err:
                entry['commit']={'code':err.code,'transaction':err.transaction,'message':str(err)}
            entry['after_commit']={'sink':db.all('SELECT count(*) FROM sink'),'prior':db.all('SELECT * FROM prior'),'state':db.transaction_state}
            results.append(entry)
print(json.dumps(results,indent=2))
