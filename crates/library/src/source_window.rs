//! Shared ordering and bounded reads for source projections.
use crate::LibraryResult;
use sqlx::SqliteConnection;
pub(crate) fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

pub(crate) struct SourceQuery {
    pub from: String,
    pub predicate: String,
    pub order: Vec<String>,
    pub uri: String,
    pub entry_key: String,
}

impl SourceQuery {
    pub async fn count(&self, connection: &mut SqliteConnection) -> LibraryResult<i64> {
        Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT count(*) FROM {} WHERE {}",
            self.from, self.predicate
        )))
        .fetch_one(connection)
        .await?)
    }

    pub fn select(&self, columns: &str) -> String {
        format!(
            "SELECT {columns} FROM {} WHERE {} ORDER BY {}",
            self.from,
            self.predicate,
            self.order.join(",")
        )
    }
}

pub(crate) async fn selected_values_on<T>(
    connection: &mut SqliteConnection,
    query: &SourceQuery,
    column: &str,
    ranges: &[std::ops::Range<usize>],
) -> LibraryResult<Vec<T>>
where
    T: for<'r> sqlx::Decode<'r, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite> + Send + Unpin,
{
    Ok(
        selected_rows_on::<(T,)>(connection, query, column, Some(ranges))
            .await?
            .into_iter()
            .map(|(value,)| value)
            .collect(),
    )
}

pub(crate) async fn selected_rows_on<T>(
    connection: &mut SqliteConnection,
    query: &SourceQuery,
    columns: &str,
    ranges: Option<&[std::ops::Range<usize>]>,
) -> LibraryResult<Vec<T>>
where
    T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow> + Send + Unpin,
{
    let Some(ranges) = ranges else {
        return Ok(sqlx::query_as(sqlx::AssertSqlSafe(query.select(columns)))
            .persistent(false)
            .fetch_all(connection)
            .await?);
    };
    let Some(first) = ranges.first() else {
        return Ok(Vec::new());
    };
    if ranges.len() == 1 {
        return Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "{} LIMIT ?1 OFFSET ?2",
            query.select(columns)
        )))
        .bind(first.len().min(i64::MAX as usize) as i64)
        .bind(first.start.min(i64::MAX as usize) as i64)
        .persistent(false)
        .fetch_all(connection)
        .await?);
    }
    let columns = format!(
        "{columns}, row_number() OVER (ORDER BY {})-1 selected_position",
        query.order.join(",")
    );
    let sql = format!("WITH ordered AS ({} LIMIT ?1)
         SELECT * FROM ordered WHERE EXISTS (
           SELECT 1 FROM json_each(?2) selected
           WHERE selected_position>=json_extract(selected.value,'$.start')
             AND selected_position<json_extract(selected.value,'$.end')) ORDER BY selected_position", query.select(&columns));
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(
            ranges
                .iter()
                .map(|range| range.end)
                .max()
                .unwrap_or(0)
                .min(i64::MAX as usize) as i64,
        )
        .bind(serde_json::to_string(ranges)?)
        .persistent(false)
        .fetch_all(connection)
        .await?)
}
