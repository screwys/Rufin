// Keep each bounded row's fields and SQL expressions in the same declaration.
macro_rules! row_projection {
    (
        $(#[$attribute:meta])*
        $visibility:vis struct $name:ident {
            $($(#[$field_attribute:meta])* pub $field:ident: $ty:ty = $expression:expr,)*
        }
    ) => {
        $(#[$attribute])*
        $visibility struct $name {
            $($(#[$field_attribute])* pub $field: $ty,)*
        }

        impl $name {
            pub(crate) fn columns() -> &'static str {
                static SQL: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
                    [$(format!("{} AS {}", $expression, stringify!($field)),)*].join(",")
                });
                SQL.as_str()
            }
        }
    };
}
pub(crate) use row_projection;
