-- Diagnostic for the pinned engine; run in a fresh database.
CREATE TABLE docs;
CREATE TABLE baseline(n INTEGER);
INSERT INTO docs {n:1};
INSERT INTO docs {n:2};
INSERT INTO baseline VALUES(1),(2);
-- Native computed zero LIMIT currently admits both scalar rows.
SELECT n,(SELECT d.n ORDER BY d.n DESC LIMIT 0+0 OFFSET 0+0)
FROM baseline d ORDER BY n;
-- Logical computed zero LIMIT suppresses both scalar rows.
SELECT n,(SELECT array::new(d.n) ORDER BY d.n DESC LIMIT 0+0 OFFSET 0+0)
FROM docs d ORDER BY n;
-- Logical computed OFFSET now suppresses both scalar rows.
-- Before counter isolation, it suppressed only the first scalar row.
SELECT n,(SELECT array::new(d.n) ORDER BY d.n DESC LIMIT 1+0 OFFSET 1+0)
FROM docs d ORDER BY n;
