// Copyright: Ankitects Pty Ltd and contributors
// License: GNU AGPL, version 3 or later; http://www.gnu.org/licenses/agpl.html

use crate::backend_proto::{DbResult, Row, SqlValue as pb_SqlValue, sql_value::Data};
use crate::backend_proto;
use crate::err::Result;
use crate::storage::SqliteStorage;
use rusqlite::types::{FromSql, FromSqlError, ToSql, ToSqlOutput, ValueRef};
use rusqlite::OptionalExtension;
use serde_derive::{Deserialize, Serialize};

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub(super) enum DBRequest {
    Query {
        sql: String,
        args: Vec<SqlValue>,
        first_row_only: bool,
    },
    Begin,
    Commit,
    Rollback,
    ExecuteMany {
        sql: String,
        args: Vec<Vec<SqlValue>>,
    },
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum DBResult {
    Rows(Vec<Vec<SqlValue>>),
    None,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(untagged)]
pub enum SqlValue {
    Null,
    String(String),
    Int(i64),
    Double(f64),
    Blob(Vec<u8>),
}

impl ToSql for SqlValue {
    fn to_sql(&self) -> std::result::Result<ToSqlOutput<'_>, rusqlite::Error> {
        let val = match self {
            SqlValue::Null => ValueRef::Null,
            SqlValue::String(v) => ValueRef::Text(v.as_bytes()),
            SqlValue::Int(v) => ValueRef::Integer(*v),
            SqlValue::Double(v) => ValueRef::Real(*v),
            SqlValue::Blob(v) => ValueRef::Blob(&v),
        };
        Ok(ToSqlOutput::Borrowed(val))
    }
}

impl From<&SqlValue> for backend_proto::SqlValue {
    fn from(item: &SqlValue) -> Self {
        match item {
            SqlValue::Null => pb_SqlValue { data: Option::None },
            SqlValue::String(s) => pb_SqlValue { data: Some(Data::StringValue(s.to_string())) },
            SqlValue::Int(i) => pb_SqlValue { data: Some(Data::LongValue(*i)) },
            SqlValue::Double(d) => pb_SqlValue { data: Some(Data::DoubleValue(*d)) },
            SqlValue::Blob(b) => pb_SqlValue { data: Some(Data::BlobValue(b.clone())) },
        }
    }
}

impl From<&Vec<SqlValue>> for backend_proto::Row {
    fn from(item: &Vec<SqlValue>) -> Self {
        Row { fields : item.iter().map(|y| backend_proto::SqlValue::from(y)).collect() }
    }
}

impl From<&Vec<Vec<SqlValue>>> for backend_proto::DbResult {
    fn from(item: &Vec<Vec<SqlValue>>) -> Self {
        DbResult { rows: item.iter().map(|x| Row::from(x)).collect() }
    }
}

impl FromSql for SqlValue {
    fn column_result(value: ValueRef<'_>) -> std::result::Result<Self, FromSqlError> {
        let val = match value {
            ValueRef::Null => SqlValue::Null,
            ValueRef::Integer(i) => SqlValue::Int(i),
            ValueRef::Real(v) => SqlValue::Double(v),
            ValueRef::Text(v) => SqlValue::String(String::from_utf8_lossy(v).to_string()),
            ValueRef::Blob(v) => SqlValue::Blob(v.to_vec()),
        };
        Ok(val)
    }
}

pub(super) fn db_command_bytes(ctx: &SqliteStorage, input: &[u8]) -> Result<Vec<u8>> {
    let req: DBRequest = serde_json::from_slice(input)?;
    let resp = match req {
        DBRequest::Query {
            sql,
            args,
            first_row_only,
        } => {
            if first_row_only {
                db_query_row(ctx, &sql, &args)?
            } else {
                db_query(ctx, &sql, &args)?
            }
        }
        DBRequest::Begin => {
            ctx.begin_trx()?;
            DBResult::None
        }
        DBRequest::Commit => {
            ctx.commit_trx()?;
            DBResult::None
        }
        DBRequest::Rollback => {
            ctx.rollback_trx()?;
            DBResult::None
        }
        DBRequest::ExecuteMany { sql, args } => db_execute_many(ctx, &sql, &args)?,
    };
    Ok(serde_json::to_vec(&resp)?)
}

pub(super) fn db_command_proto(ctx: &SqliteStorage, input: &[u8]) -> Result<DbResult> {
    let req: DBRequest = serde_json::from_slice(input)?;
    let resp = match req {
        DBRequest::Query {
            sql,
            args,
            first_row_only,
        } => {
            if first_row_only {
                db_query_row(ctx, &sql, &args)?
            } else {
                db_query(ctx, &sql, &args)?
            }
        }
        DBRequest::Begin => {
            ctx.begin_trx()?;
            DBResult::None
        }
        DBRequest::Commit => {
            ctx.commit_trx()?;
            DBResult::None
        }
        DBRequest::Rollback => {
            ctx.rollback_trx()?;
            DBResult::None
        }
        DBRequest::ExecuteMany { sql, args } => db_execute_many(ctx, &sql, &args)?,
    };
    let proto_resp = match resp {
        DBResult::None => DbResult { rows: Vec::new() },
        DBResult::Rows(rows) => DbResult::from(&rows)
    };
    Ok(proto_resp)
}

pub(super) fn db_query_row(ctx: &SqliteStorage, sql: &str, args: &[SqlValue]) -> Result<DBResult> {
    let mut stmt = ctx.db.prepare_cached(sql)?;
    let columns = stmt.column_count();

    let row = stmt
        .query_row(args, |row| {
            let mut orow = Vec::with_capacity(columns);
            for i in 0..columns {
                let v: SqlValue = row.get(i)?;
                orow.push(v);
            }
            Ok(orow)
        })
        .optional()?;

    let rows = if let Some(row) = row {
        vec![row]
    } else {
        vec![]
    };

    Ok(DBResult::Rows(rows))
}

pub(super) fn db_query(ctx: &SqliteStorage, sql: &str, args: &[SqlValue]) -> Result<DBResult> {
    let mut stmt = ctx.db.prepare_cached(sql)?;
    let columns = stmt.column_count();

    let res: std::result::Result<Vec<Vec<_>>, rusqlite::Error> = stmt
        .query_map(args, |row| {
            let mut orow = Vec::with_capacity(columns);
            for i in 0..columns {
                let v: SqlValue = row.get(i)?;
                orow.push(v);
            }
            Ok(orow)
        })?
        .collect();

    Ok(DBResult::Rows(res?))
}

pub(super) fn db_execute_many(
    ctx: &SqliteStorage,
    sql: &str,
    args: &[Vec<SqlValue>],
) -> Result<DBResult> {
    let mut stmt = ctx.db.prepare_cached(sql)?;

    for params in args {
        stmt.execute(params)?;
    }

    Ok(DBResult::None)
}
