//! Steps one statement on a connection's raw SQLite handle. The load reads every thin row of
//! the Store, and handing each row from sqlx's connection worker to the caller costs far more
//! than reading it.
#![allow(unsafe_code)]

use std::ffi::{CStr, c_int};
use std::ptr::NonNull;

use libsqlite3_sys as ffi;
use sqlx::SqliteConnection;

use crate::{LibraryError, LibraryResult};

/// A connection's handle, locked so sqlx's worker leaves it alone.
pub(super) struct Handle<'a> {
    // Keeps sqlx's worker off the handle while statements step on it.
    _locked: sqlx::sqlite::LockedSqliteHandle<'a>,
    raw: NonNull<ffi::sqlite3>,
}

impl<'a> Handle<'a> {
    pub(super) async fn lock(connection: &'a mut SqliteConnection) -> LibraryResult<Self> {
        let mut locked = connection.lock_handle().await?;
        let raw = locked.as_raw_handle();
        Ok(Self {
            _locked: locked,
            raw,
        })
    }

    pub(super) fn prepare(&self, sql: &str) -> LibraryResult<Rows<'_>> {
        let mut statement = std::ptr::null_mut();
        // SAFETY: the handle is locked and open; SQLite copies nothing past the given length.
        let code = unsafe {
            ffi::sqlite3_prepare_v2(
                self.raw.as_ptr(),
                sql.as_ptr().cast(),
                c_int::try_from(sql.len()).unwrap_or(c_int::MAX),
                &mut statement,
                std::ptr::null_mut(),
            )
        };
        self.check(code)?;
        NonNull::new(statement)
            .map(|statement| Rows {
                handle: self,
                statement,
            })
            .ok_or_else(|| LibraryError::InvalidStore("an empty load statement".into()))
    }

    fn check(&self, code: c_int) -> LibraryResult<()> {
        if code == ffi::SQLITE_OK {
            return Ok(());
        }
        // SAFETY: the handle is open, and SQLite returns a static or connection-owned string.
        let message = unsafe { CStr::from_ptr(ffi::sqlite3_errmsg(self.raw.as_ptr())) };
        Err(LibraryError::InvalidStore(format!(
            "the Catalog load failed: {}",
            message.to_string_lossy()
        )))
    }
}

/// A prepared statement and its current row.
pub(super) struct Rows<'h> {
    handle: &'h Handle<'h>,
    statement: NonNull<ffi::sqlite3_stmt>,
}

impl Rows<'_> {
    pub(super) fn bind(&mut self, index: c_int, value: i64) -> LibraryResult<&mut Self> {
        // SAFETY: the statement is live and SQLite checks the index.
        let code = unsafe { ffi::sqlite3_bind_int64(self.statement.as_ptr(), index, value) };
        self.handle.check(code)?;
        Ok(self)
    }

    /// Moves to the next row; false when there is none.
    pub(super) fn step(&mut self) -> LibraryResult<bool> {
        // SAFETY: the statement is live and steps on the locked handle.
        match unsafe { ffi::sqlite3_step(self.statement.as_ptr()) } {
            ffi::SQLITE_ROW => Ok(true),
            ffi::SQLITE_DONE => Ok(false),
            code => self.handle.check(code).map(|()| false),
        }
    }

    pub(super) fn null(&self, column: c_int) -> bool {
        // SAFETY: the statement is on a row and SQLite checks the column.
        unsafe { ffi::sqlite3_column_type(self.statement.as_ptr(), column) == ffi::SQLITE_NULL }
    }

    pub(super) fn int(&self, column: c_int) -> i64 {
        // SAFETY: the statement is on a row and SQLite checks the column.
        unsafe { ffi::sqlite3_column_int64(self.statement.as_ptr(), column) }
    }

    pub(super) fn optional_int(&self, column: c_int) -> Option<i64> {
        (!self.null(column)).then(|| self.int(column))
    }

    pub(super) fn flag(&self, column: c_int) -> bool {
        self.int(column) != 0
    }

    /// Text that lives until the next step. Text that is not UTF-8 reads as empty.
    pub(super) fn text(&self, column: c_int) -> &str {
        // SAFETY: the statement is on a row; SQLite returns the column's text and its length,
        // which stay valid until the next step, reset or finalize, all of which take `&mut`.
        unsafe {
            let text = ffi::sqlite3_column_text(self.statement.as_ptr(), column);
            if text.is_null() {
                return "";
            }
            let len = ffi::sqlite3_column_bytes(self.statement.as_ptr(), column);
            let bytes = std::slice::from_raw_parts(text, usize::try_from(len).unwrap_or(0));
            std::str::from_utf8(bytes).unwrap_or_default()
        }
    }

    pub(super) fn optional_text(&self, column: c_int) -> Option<&str> {
        (!self.null(column)).then(|| self.text(column))
    }

    pub(super) fn blob(&self, column: c_int) -> Option<Vec<u8>> {
        if self.null(column) {
            return None;
        }
        // SAFETY: as for text; the bytes are copied before the next step.
        unsafe {
            let blob = ffi::sqlite3_column_blob(self.statement.as_ptr(), column);
            let len = ffi::sqlite3_column_bytes(self.statement.as_ptr(), column);
            if blob.is_null() {
                return Some(Vec::new());
            }
            Some(
                std::slice::from_raw_parts(blob.cast::<u8>(), usize::try_from(len).unwrap_or(0))
                    .to_vec(),
            )
        }
    }
}

impl Drop for Rows<'_> {
    fn drop(&mut self) {
        // SAFETY: the statement was prepared on this handle and is finalized once.
        unsafe {
            ffi::sqlite3_finalize(self.statement.as_ptr());
        }
    }
}
