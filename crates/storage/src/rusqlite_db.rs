//! rusqlite-backed [`Db`] for native builds and tests.

use std::sync::Mutex;

use rusqlite::types::Value as SqliteValue;
use rusqlite::{Connection as SqliteConnection, params_from_iter};

use crate::StorageError;
use crate::db::{Db, DbValue, ExecResult, QueryRow};

/// rusqlite-backed [`Db`].
///
/// `rusqlite::Connection` is `!Sync`, so it is held behind a `Mutex`.
pub struct RusqliteDb {
    conn: std::sync::Arc<Mutex<Option<SqliteConnection>>>,
    // A transaction owns the connection across awaits. Other callers must wait
    // without blocking its executor thread or entering its uncommitted state.
    access: futures::lock::Mutex<()>,
}

impl RusqliteDb {
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self, StorageError> {
        let conn = SqliteConnection::open(path).map_err(|e| StorageError::Db(e.to_string()))?;
        conn.execute("PRAGMA foreign_keys = ON", [])
            .map_err(|e| StorageError::Db(e.to_string()))?;
        Ok(Self {
            conn: std::sync::Arc::new(Mutex::new(Some(conn))),
            access: futures::lock::Mutex::new(()),
        })
    }

    pub fn open_in_memory() -> Result<Self, StorageError> {
        let conn =
            SqliteConnection::open_in_memory().map_err(|e| StorageError::Db(e.to_string()))?;
        conn.execute("PRAGMA foreign_keys = ON", [])
            .map_err(|e| StorageError::Db(e.to_string()))?;
        Ok(Self {
            conn: std::sync::Arc::new(Mutex::new(Some(conn))),
            access: futures::lock::Mutex::new(()),
        })
    }
}

fn to_sqlite_value(v: &DbValue) -> SqliteValue {
    match v {
        DbValue::Null => SqliteValue::Null,
        DbValue::Int(i) => SqliteValue::Integer(*i),
        DbValue::Real(f) => SqliteValue::Real(*f),
        DbValue::Text(t) => SqliteValue::Text(t.clone()),
        DbValue::Blob(b) => SqliteValue::Blob(b.clone()),
    }
}

fn to_db_value(v: SqliteValue) -> DbValue {
    match v {
        SqliteValue::Null => DbValue::Null,
        SqliteValue::Integer(i) => DbValue::Int(i),
        SqliteValue::Real(f) => DbValue::Real(f),
        SqliteValue::Text(t) => DbValue::Text(t),
        SqliteValue::Blob(b) => DbValue::Blob(b),
    }
}

fn execute_connection(
    conn: &SqliteConnection,
    sql: &str,
    params: &[DbValue],
) -> Result<ExecResult, StorageError> {
    let values: Vec<SqliteValue> = params.iter().map(to_sqlite_value).collect();

    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| StorageError::Db(e.to_string()))?;
    let column_count = stmt.column_count();

    let rows = stmt
        .query_map(params_from_iter(values.iter()), |row| {
            let mut values = Vec::with_capacity(column_count);
            for i in 0..column_count {
                let v = row.get::<usize, SqliteValue>(i)?;
                values.push(to_db_value(v));
            }
            Ok(QueryRow { values })
        })
        .map_err(|e| match e {
            rusqlite::Error::SqliteFailure(err, Some(msg))
                if err.code == rusqlite::ErrorCode::ConstraintViolation
                    && (err.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
                        || err.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY) =>
            {
                StorageError::Conflict(msg)
            }
            _ => StorageError::Db(e.to_string()),
        })?;

    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| match e {
            rusqlite::Error::SqliteFailure(err, Some(msg))
                if err.code == rusqlite::ErrorCode::ConstraintViolation
                    && (err.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
                        || err.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY) =>
            {
                StorageError::Conflict(msg)
            }
            _ => StorageError::Db(e.to_string()),
        })?);
    }

    Ok(ExecResult {
        last_insert_rowid: conn.last_insert_rowid(),
        changes: conn.changes(),
        rows: out,
    })
}

impl Db for RusqliteDb {
    type Tx<'a> = RusqliteTx;

    async fn execute(&self, sql: &str, params: &[DbValue]) -> Result<ExecResult, StorageError> {
        let _access = self.access.lock().await;
        let mut conn_guard = self
            .conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let conn = conn_guard
            .as_mut()
            .ok_or_else(|| StorageError::Db("Connection in use by transaction".to_string()))?;
        execute_connection(conn, sql, params)
    }

    async fn transaction<'a, F, Fut, T: 'a + Send>(&'a self, body: F) -> Result<T, StorageError>
    where
        F: FnOnce(Self::Tx<'a>) -> Fut + Send + 'a,
        Fut: std::future::Future<Output = Result<T, StorageError>> + Send + 'a,
    {
        // Declared before TxGuard so rollback/commit restores the connection
        // before a waiting caller is released, including on cancellation.
        let _access = self.access.lock().await;
        let conn = {
            let mut guard = self
                .conn
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard
                .take()
                .ok_or_else(|| StorageError::Db("Concurrent transactions not supported".into()))?
        };

        if let Err(e) = conn.execute("BEGIN IMMEDIATE", []) {
            let mut guard = self
                .conn
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *guard = Some(conn);
            return Err(StorageError::Db(e.to_string()));
        }

        let tx = RusqliteTx {
            conn: std::sync::Arc::new(std::sync::Mutex::new(Some(conn))),
        };

        struct TxGuard<'a> {
            tx_conn: std::sync::Arc<std::sync::Mutex<Option<SqliteConnection>>>,
            db_conn: &'a std::sync::Arc<Mutex<Option<SqliteConnection>>>,
            commit_requested: bool,
        }

        impl<'a> Drop for TxGuard<'a> {
            fn drop(&mut self) {
                if let Some(c) = self
                    .tx_conn
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                {
                    if self.commit_requested {
                        let _ = c.execute("COMMIT", []);
                    } else {
                        let _ = c.execute("ROLLBACK", []);
                    }
                    let mut slot_guard = self
                        .db_conn
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    *slot_guard = Some(c);
                }
            }
        }

        let mut guard = TxGuard {
            tx_conn: tx.conn.clone(),
            db_conn: &self.conn,
            commit_requested: false,
        };

        let res = body(tx).await;
        if res.is_ok() {
            guard.commit_requested = true;
        }

        res
    }
}

pub struct RusqliteTx {
    pub(crate) conn: std::sync::Arc<std::sync::Mutex<Option<SqliteConnection>>>,
}

impl Db for RusqliteTx {
    type Tx<'b> = RusqliteTx;

    async fn execute(&self, sql: &str, params: &[DbValue]) -> Result<ExecResult, StorageError> {
        let mut conn_guard = self
            .conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let conn = conn_guard
            .as_mut()
            .ok_or_else(|| StorageError::Db("Connection in use by transaction".to_string()))?;
        execute_connection(conn, sql, params)
    }

    async fn transaction<'b, F, Fut, T: 'b>(&'b self, _body: F) -> Result<T, StorageError>
    where
        F: FnOnce(Self::Tx<'b>) -> Fut + Send + 'b,
        Fut: std::future::Future<Output = Result<T, StorageError>> + Send + 'b,
    {
        Err(StorageError::Db(
            "Nested transactions are not supported".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{channel::oneshot, executor::block_on, poll};

    #[test]
    fn reads_and_transactions_wait_for_the_current_transaction() {
        block_on(async {
            let db = RusqliteDb::open_in_memory().unwrap();
            db.execute("CREATE TABLE entries (id INTEGER)", &[])
                .await
                .unwrap();
            let (release, held) = oneshot::channel();
            let mut first = Box::pin(db.transaction(|tx| async move {
                tx.execute("INSERT INTO entries VALUES (1)", &[]).await?;
                held.await.unwrap();
                Ok(())
            }));
            assert!(poll!(first.as_mut()).is_pending());

            let mut read = Box::pin(db.execute("SELECT id FROM entries WHERE id = 1", &[]));
            let observed = poll!(read.as_mut());
            assert!(
                observed.is_pending(),
                "A read must wait for the owning transaction: {observed:?}"
            );
            let mut second = Box::pin(db.transaction(|tx| async move {
                tx.execute("INSERT INTO entries VALUES (2)", &[]).await?;
                Ok(())
            }));
            let observed = poll!(second.as_mut());
            assert!(
                observed.is_pending(),
                "A second transaction must wait for the first: {observed:?}"
            );

            release.send(()).unwrap();
            first.await.unwrap();
            let (read, second) = futures::join!(read, second);
            assert_eq!(read.unwrap().rows[0].get_int(0).unwrap(), 1);
            second.unwrap();
            let total = db
                .execute("SELECT sum(id) FROM entries", &[])
                .await
                .unwrap();
            assert_eq!(total.rows[0].get_int(0).unwrap(), 3);
        });
    }

    #[test]
    fn cancelling_a_transaction_rolls_back_before_releasing_waiters() {
        block_on(async {
            let db = RusqliteDb::open_in_memory().unwrap();
            db.execute("CREATE TABLE entries (id INTEGER)", &[])
                .await
                .unwrap();
            let mut first = Box::pin(db.transaction(|tx| async move {
                tx.execute("INSERT INTO entries VALUES (1)", &[]).await?;
                futures::future::pending::<()>().await;
                Ok(())
            }));
            assert!(poll!(first.as_mut()).is_pending());
            let mut read = Box::pin(db.execute("SELECT count(*) FROM entries", &[]));
            let observed = poll!(read.as_mut());
            assert!(
                observed.is_pending(),
                "A read must not enter an unfinished transaction: {observed:?}"
            );
            drop(first);
            assert_eq!(read.await.unwrap().rows[0].get_int(0).unwrap(), 0);
            db.transaction(|tx| async move {
                tx.execute("INSERT INTO entries VALUES (2)", &[]).await?;
                Ok(())
            })
            .await
            .unwrap();
            let total = db
                .execute("SELECT sum(id) FROM entries", &[])
                .await
                .unwrap();
            assert_eq!(total.rows[0].get_int(0).unwrap(), 2);
        });
    }
}
