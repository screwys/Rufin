//! Word matching with positional characters for lyrics written without word separators.
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};
use std::ptr;

use libsqlite3_sys as sqlite;

use crate::{LibraryError, LibraryResult};

pub(crate) fn character_token(character: char) -> bool {
    character.is_alphabetic()
        && matches!(character as u32,
            0x3040..=0x30ff | 0x31f0..=0x31ff | 0x3400..=0x4dbf |
            0x4e00..=0x9fff | 0xac00..=0xd7af | 0xf900..=0xfaff |
            0xff66..=0xff9d | 0x20000..=0x323af)
}

struct Factory {
    methods: sqlite::fts5_tokenizer,
    context: *mut c_void,
}

struct Tokenizer {
    methods: sqlite::fts5_tokenizer,
    inner: *mut sqlite::Fts5Tokenizer,
}

type Emit = unsafe extern "C" fn(*mut c_void, c_int, *const c_char, c_int, c_int, c_int) -> c_int;

struct Output {
    context: *mut c_void,
    emit: Emit,
    offset: c_int,
}

// SQLite calls these functions synchronously while the connection and tokenizer are alive.
unsafe extern "C" fn create(
    context: *mut c_void,
    _arguments: *mut *const c_char,
    _count: c_int,
    output: *mut *mut sqlite::Fts5Tokenizer,
) -> c_int {
    unsafe {
        let factory = &*context.cast::<Factory>();
        let mut inner = ptr::null_mut();
        let mut arguments = [c"remove_diacritics".as_ptr(), c"2".as_ptr()];
        let result = factory.methods.xCreate.unwrap()(
            factory.context,
            arguments.as_mut_ptr(),
            arguments.len() as c_int,
            &mut inner,
        );
        if result == sqlite::SQLITE_OK {
            *output = Box::into_raw(Box::new(Tokenizer {
                methods: factory.methods,
                inner,
            }))
            .cast();
        }
        result
    }
}

unsafe extern "C" fn destroy(tokenizer: *mut sqlite::Fts5Tokenizer) {
    unsafe {
        let tokenizer = Box::from_raw(tokenizer.cast::<Tokenizer>());
        tokenizer.methods.xDelete.unwrap()(tokenizer.inner);
    }
}

unsafe extern "C" fn destroy_factory(context: *mut c_void) {
    unsafe { drop(Box::from_raw(context.cast::<Factory>())) };
}

unsafe extern "C" fn forward(
    context: *mut c_void,
    flags: c_int,
    token: *const c_char,
    length: c_int,
    start: c_int,
    end: c_int,
) -> c_int {
    unsafe {
        let output = &*context.cast::<Output>();
        (output.emit)(
            output.context,
            flags,
            token,
            length,
            start + output.offset,
            end + output.offset,
        )
    }
}

unsafe extern "C" fn tokenize(
    tokenizer: *mut sqlite::Fts5Tokenizer,
    context: *mut c_void,
    flags: c_int,
    text: *const c_char,
    length: c_int,
    emit: Option<Emit>,
) -> c_int {
    if length == 0 {
        return sqlite::SQLITE_OK;
    }
    unsafe {
        let Some(emit) = emit else {
            return sqlite::SQLITE_ERROR;
        };
        let bytes = std::slice::from_raw_parts(text.cast::<u8>(), length as usize);
        let Ok(value) = std::str::from_utf8(bytes) else {
            return sqlite::SQLITE_ERROR;
        };
        let tokenizer = &*tokenizer.cast::<Tokenizer>();
        let mut output = Output {
            context,
            emit,
            offset: 0,
        };
        let mut start = 0;
        for (offset, character) in value.char_indices() {
            if !character_token(character) {
                continue;
            }
            if start < offset {
                output.offset = start as c_int;
                let result = tokenizer.methods.xTokenize.unwrap()(
                    tokenizer.inner,
                    (&mut output as *mut Output).cast(),
                    flags,
                    text.add(start),
                    (offset - start) as c_int,
                    Some(forward),
                );
                if result != sqlite::SQLITE_OK {
                    return result;
                }
            }
            start = offset + character.len_utf8();
            let result = emit(
                context,
                0,
                text.add(offset),
                character.len_utf8() as c_int,
                offset as c_int,
                start as c_int,
            );
            if result != sqlite::SQLITE_OK {
                return result;
            }
        }
        if start < value.len() {
            output.offset = start as c_int;
            return tokenizer.methods.xTokenize.unwrap()(
                tokenizer.inner,
                (&mut output as *mut Output).cast(),
                flags,
                text.add(start),
                length - start as c_int,
                Some(forward),
            );
        }
        sqlite::SQLITE_OK
    }
}

pub(crate) async fn register(connection: &mut sqlx::SqliteConnection) -> LibraryResult<()> {
    let mut handle = connection.lock_handle().await?;
    // SAFETY: SQLx excludes its worker while the handle guard is held. SQLite owns
    // the factory after registration and releases it with destroy_factory.
    unsafe {
        let database = handle.as_raw_handle().as_ptr();
        let mut statement = ptr::null_mut();
        let mut api: *mut sqlite::fts5_api = ptr::null_mut();
        let mut result = sqlite::sqlite3_prepare_v2(
            database,
            c"SELECT fts5(?1)".as_ptr(),
            -1,
            &mut statement,
            ptr::null_mut(),
        );
        if result == sqlite::SQLITE_OK {
            result = sqlite::sqlite3_bind_pointer(
                statement,
                1,
                (&mut api as *mut *mut sqlite::fts5_api).cast(),
                c"fts5_api_ptr".as_ptr(),
                None,
            );
            if result == sqlite::SQLITE_OK {
                result = sqlite::sqlite3_step(statement);
            }
        }
        sqlite::sqlite3_finalize(statement);
        if result != sqlite::SQLITE_ROW || api.is_null() {
            return Err(LibraryError::InvalidStore(
                "SQLite FTS5 is unavailable".into(),
            ));
        }
        let mut factory = Box::new(Factory {
            methods: std::mem::zeroed(),
            context: ptr::null_mut(),
        });
        let result = (*api).xFindTokenizer.unwrap()(
            api,
            c"unicode61".as_ptr(),
            &mut factory.context,
            &mut factory.methods,
        );
        if result != sqlite::SQLITE_OK {
            return Err(LibraryError::InvalidStore(
                "SQLite Unicode tokenizer is unavailable".into(),
            ));
        }
        let factory = Box::into_raw(factory).cast();
        let mut methods = sqlite::fts5_tokenizer {
            xCreate: Some(create),
            xDelete: Some(destroy),
            xTokenize: Some(tokenize),
        };
        let result = (*api).xCreateTokenizer.unwrap()(
            api,
            c"rufin_lyrics".as_ptr(),
            factory,
            &mut methods,
            Some(destroy_factory),
        );
        if result != sqlite::SQLITE_OK {
            destroy_factory(factory);
            return Err(LibraryError::InvalidStore(
                "Could not register lyrics tokenizer".into(),
            ));
        }
    }
    Ok(())
}
