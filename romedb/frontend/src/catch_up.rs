//! Bring a private copy with a new index up to date with the database it was
//! copied from. A long index build runs on a copy while the database takes
//! writes; afterwards only the rows that changed are copied in, and only the new
//! index is maintained for them.
use crate::{
    canonical, quote, Collection, Connection, Document, EngineValue, Error, Index, Result,
};
use std::collections::BTreeSet;

/// What a catch-up changed in the copy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CatchUp {
    pub tables: u64,
    pub rows_deleted: u64,
    pub rows_inserted: u64,
    pub documents: u64,
}

const PAGE_ROWS: usize = 1024;

impl Connection {
    /// `self` is a private copy of `source` with one more index, `index`,
    /// built while `source` took writes. This copies every row that differs
    /// into `self`, by rowid, and maintains only the new index for the
    /// documents that changed: every other managed table is copied as it is.
    /// The schema must not have changed in any other way.
    pub fn catch_up_from(&self, source: &Connection, index: &str) -> Result<CatchUp> {
        let index = canonical(index)?;
        // A managed index is maintained here for the documents that changed. An
        // index on a plain table follows the rows copied into that table.
        let managed = self.catch_up_index(&index)?;
        self.require_same_catalog(source, managed.as_ref())?;
        let new_objects = self.require_same_schema(source)?;
        if managed.is_none() && !new_objects.contains(&index) {
            return Err(Error::NotFound(format!("index {index}")));
        }
        self.atomic(|| {
            let mut report = CatchUp::default();
            for (table, without_rowid) in source.catch_up_tables()? {
                if new_objects.contains(&table) {
                    return Err(changed_schema(&table));
                }
                if without_rowid {
                    if source.whole_table(&table)? != self.whole_table(&table)? {
                        return Err(Error::Validation(format!(
                            "{table} changed during the index build, and a table without rowids cannot be caught up"
                        )));
                    }
                    continue;
                }
                let difference = self.row_difference(source, &table)?;
                if difference.is_empty() {
                    continue;
                }
                report.tables += 1;
                let new = managed
                    .as_ref()
                    .filter(|(owner, _)| owner.storage == table)
                    .map(|(_, new)| new);
                for (rowid, row) in &difference.removed {
                    if let Some(new) = new {
                        self.remove_new_index_entry(new, row)?;
                    }
                    self.run(
                        &format!("DELETE FROM {} WHERE rowid = ?1", quote(&table)),
                        &[EngineValue::Numeric(turso_core::Numeric::Integer(*rowid))],
                    )?;
                    report.rows_deleted += 1;
                }
                for (rowid, row) in &difference.added {
                    self.insert_row(&table, &difference.columns, *rowid, row)?;
                    if let Some(new) = new {
                        self.insert_index(new, &document_of(row)?)?;
                        report.documents += 1;
                    }
                    report.rows_inserted += 1;
                }
            }
            Ok(report)
        })
    }

    fn catch_up_index(&self, name: &str) -> Result<Option<(Collection, Index)>> {
        Ok(self.collections()?.into_iter().find_map(|collection| {
            let index = collection
                .indexes
                .iter()
                .find(|index| index.name == name)?
                .clone();
            Some((collection, index))
        }))
    }

    fn require_same_catalog(
        &self,
        source: &Connection,
        managed: Option<&(Collection, Index)>,
    ) -> Result<()> {
        let mut mine = self.collections()?;
        if let Some((owner, new)) = managed {
            for collection in &mut mine {
                if collection.name == owner.name {
                    collection.indexes.retain(|index| index.name != new.name);
                }
            }
        }
        let theirs = source.collections()?;
        let json = |value: &[Collection]| {
            serde_json::to_value(value).map_err(|e| Error::Storage(e.to_string()))
        };
        if json(&mine)? != json(&theirs)? {
            return Err(Error::Validation(
                "the catalog changed during the index build".into(),
            ));
        }
        Ok(())
    }

    /// Every schema object of `source` must be in `self` as it is. What only
    /// `self` has is the new index, and its tables are not copied.
    fn require_same_schema(&self, source: &Connection) -> Result<BTreeSet<String>> {
        let theirs = source.schema_objects()?;
        let mine = self.schema_objects()?;
        if !theirs.is_subset(&mine) {
            let changed = theirs
                .difference(&mine)
                .next()
                .map(|object| object.1.clone())
                .unwrap_or_default();
            return Err(changed_schema(&changed));
        }
        Ok(mine
            .difference(&theirs)
            .map(|object| object.1.clone())
            .collect())
    }

    fn schema_objects(&self) -> Result<BTreeSet<(String, String, String, String)>> {
        self.run(
            "SELECT type, name, tbl_name, coalesce(sql, '') FROM sqlite_schema",
            &[],
        )?
        .into_iter()
        .map(|row| match row.as_slice() {
            [EngineValue::Text(kind), EngineValue::Text(name), EngineValue::Text(table), EngineValue::Text(sql)] => Ok((
                kind.as_str().to_owned(),
                name.as_str().to_owned(),
                table.as_str().to_owned(),
                sql.as_str().to_owned(),
            )),
            _ => Err(Error::Storage("invalid schema row".into())),
        })
        .collect()
    }

    /// The tables whose rows are copied. A native index method keeps its own
    /// tables from the rows of the table it is on, and the catalog was compared.
    fn catch_up_tables(&self) -> Result<Vec<(String, bool)>> {
        self.run(
            "SELECT name, coalesce(sql, '') FROM sqlite_schema WHERE type = 'table' ORDER BY name",
            &[],
        )?
        .into_iter()
        .filter_map(|row| match row.as_slice() {
            [EngineValue::Text(name), EngineValue::Text(sql)] => {
                let name = name.as_str();
                let skipped = name.starts_with(turso_core::schema::TURSO_INTERNAL_PREFIX)
                    || name == "__romedb_catalog"
                    || name.starts_with("sqlite_stat")
                    || name == "sqlite_schema"
                    || name == "sqlite_master";
                (!skipped).then(|| {
                    Ok((
                        name.to_owned(),
                        sql.as_str().to_ascii_uppercase().contains("WITHOUT ROWID"),
                    ))
                })
            }
            _ => Some(Err(Error::Storage("invalid schema row".into()))),
        })
        .collect()
    }

    fn whole_table(&self, table: &str) -> Result<Vec<Vec<EngineValue>>> {
        self.run(&format!("SELECT * FROM {} ORDER BY 1", quote(table)), &[])
    }

    /// Rows only in `source`, or different there, and rows only in `self`, by rowid.
    fn row_difference(&self, source: &Connection, table: &str) -> Result<Difference> {
        let columns = self.copied_columns(table)?;
        if columns != source.copied_columns(table)? {
            return Err(changed_schema(table));
        }
        let list = std::iter::once("rowid".to_owned())
            .chain(columns.iter().map(|column| quote(column)))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT {list} FROM {} WHERE rowid > ?1 ORDER BY rowid LIMIT {PAGE_ROWS}",
            quote(table)
        );
        let mut difference = Difference {
            columns,
            ..Default::default()
        };
        let mut mine = Pages::new(self, &sql);
        let mut theirs = Pages::new(source, &sql);
        loop {
            match (mine.peek()?, theirs.peek()?) {
                (None, None) => return Ok(difference),
                (Some(_), None) => difference.removed.push(mine.next_row()),
                (None, Some(_)) => difference.added.push(theirs.next_row()),
                (Some(a), Some(b)) if a.0 < b.0 => difference.removed.push(mine.next_row()),
                (Some(a), Some(b)) if a.0 > b.0 => difference.added.push(theirs.next_row()),
                (Some(a), Some(b)) => {
                    let same = a.1 == b.1;
                    let (old, new) = (mine.next_row(), theirs.next_row());
                    if !same {
                        difference.removed.push(old);
                        difference.added.push(new);
                    }
                }
            }
        }
    }

    /// Columns written by name. An INTEGER PRIMARY KEY is the rowid, so it is
    /// written through the rowid. A hidden or generated column cannot be copied.
    fn copied_columns(&self, table: &str) -> Result<Vec<String>> {
        let rows = self.run(&format!("PRAGMA table_xinfo({})", quote(table)), &[])?;
        let primary = rows
            .iter()
            .filter(|row| {
                !matches!(
                    row.get(5),
                    Some(EngineValue::Numeric(turso_core::Numeric::Integer(0)))
                )
            })
            .count();
        let mut columns = Vec::new();
        for row in rows {
            let (Some(EngineValue::Text(name)), Some(EngineValue::Text(kind))) =
                (row.get(1), row.get(2))
            else {
                return Err(Error::Storage(format!("invalid column of {table}")));
            };
            if !matches!(
                row.get(6),
                Some(EngineValue::Numeric(turso_core::Numeric::Integer(0))) | None
            ) {
                return Err(Error::Validation(format!(
                    "{table} has a generated or hidden column, which catch-up cannot copy"
                )));
            }
            let key = !matches!(
                row.get(5),
                Some(EngineValue::Numeric(turso_core::Numeric::Integer(0)))
            );
            if key && primary == 1 && kind.as_str().eq_ignore_ascii_case("INTEGER") {
                continue;
            }
            columns.push(name.as_str().to_owned());
        }
        Ok(columns)
    }

    fn insert_row(
        &self,
        table: &str,
        columns: &[String],
        rowid: i64,
        row: &[EngineValue],
    ) -> Result<()> {
        let names = std::iter::once("rowid".to_owned())
            .chain(columns.iter().map(|column| quote(column)))
            .collect::<Vec<_>>()
            .join(",");
        let slots = (1..=columns.len() + 1)
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let mut values = vec![EngineValue::Numeric(turso_core::Numeric::Integer(rowid))];
        values.extend(row.iter().cloned());
        self.run(
            &format!("INSERT INTO {}({names}) VALUES ({slots})", quote(table)),
            &values,
        )?;
        Ok(())
    }

    fn remove_new_index_entry(&self, index: &Index, row: &[EngineValue]) -> Result<()> {
        let document = document_of(row)?;
        let id = EngineValue::Blob(document["id"].encode()?);
        if index.keyed_maintenance() {
            self.delete_keyed_entry(index, &id, index.document_keys(&document)?)
        } else {
            self.delete_index_entry(index, &id)
        }
    }
}

#[derive(Default)]
struct Difference {
    columns: Vec<String>,
    removed: Vec<(i64, Vec<EngineValue>)>,
    added: Vec<(i64, Vec<EngineValue>)>,
}

impl Difference {
    fn is_empty(&self) -> bool {
        self.removed.is_empty() && self.added.is_empty()
    }
}

/// A table read in rowid order, one page at a time.
struct Pages<'a> {
    connection: &'a Connection,
    sql: &'a str,
    rows: std::collections::VecDeque<(i64, Vec<EngineValue>)>,
    after: i64,
    done: bool,
}

impl<'a> Pages<'a> {
    fn new(connection: &'a Connection, sql: &'a str) -> Self {
        Self {
            connection,
            sql,
            rows: Default::default(),
            after: i64::MIN,
            done: false,
        }
    }
    fn peek(&mut self) -> Result<Option<&(i64, Vec<EngineValue>)>> {
        if self.rows.is_empty() && !self.done {
            let page = self.connection.run(
                self.sql,
                &[EngineValue::Numeric(turso_core::Numeric::Integer(
                    self.after,
                ))],
            )?;
            self.done = page.len() < PAGE_ROWS;
            for mut row in page {
                let EngineValue::Numeric(turso_core::Numeric::Integer(rowid)) = row.remove(0)
                else {
                    return Err(Error::Storage("invalid rowid".into()));
                };
                self.after = rowid;
                self.rows.push_back((rowid, row));
            }
        }
        Ok(self.rows.front())
    }
    fn next_row(&mut self) -> (i64, Vec<EngineValue>) {
        self.rows.pop_front().expect("peeked a row")
    }
}

/// A row of a collection's storage table: its id and its document.
fn document_of(row: &[EngineValue]) -> Result<Document> {
    match row {
        [_, value] => match crate::decode_document(value)? {
            document if document.contains_key("id") => Ok(document),
            _ => Err(Error::Storage("document has no id".into())),
        },
        _ => Err(Error::Storage("invalid document row".into())),
    }
}

fn changed_schema(table: &str) -> Error {
    Error::Validation(format!(
        "the schema changed during the index build ({table}); build the index again"
    ))
}
