-- Diagnostic: mixed-arm collation differs by operator in the pinned engine.
CREATE TABLE docs;
INSERT INTO docs {n:1,v:'a'};
CREATE TABLE labels(m INTEGER,label TEXT COLLATE NOCASE);
INSERT INTO labels VALUES(1,'A'),(2,'1');
-- Native IS matches 'A'; native singleton IN does not.
SELECT m FROM (SELECT m,label COLLATE BINARY AS label FROM labels WHERE m<=1 UNION ALL SELECT m,label FROM labels WHERE m>1) WHERE 'a' IS label;
SELECT m FROM (SELECT m,label COLLATE BINARY AS label FROM labels WHERE m<=1 UNION ALL SELECT m,label FROM labels WHERE m>1) WHERE label IN ('a');
-- Logical results must match both native references (with n added).
SELECT n,m FROM (SELECT n,v FROM docs) CROSS JOIN (SELECT m,label COLLATE BINARY AS label FROM labels WHERE m<=1 UNION ALL SELECT m,label FROM labels WHERE m>1) WHERE v IS label;
SELECT n,m FROM (SELECT n,v FROM docs) CROSS JOIN (SELECT m,label COLLATE BINARY AS label FROM labels WHERE m<=1 UNION ALL SELECT m,label FROM labels WHERE m>1) WHERE label IN (v);
