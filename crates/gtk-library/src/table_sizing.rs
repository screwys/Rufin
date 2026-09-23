use crate::CatalogUi;
const FITTED_TABLE_WIDTH_PADDING: i32 = 2;
pub fn route_column_view_initial_width(shell: &CatalogUi) -> i32 {
    route_column_view_initial_width_with_inset(shell, 0)
}
pub fn route_column_view_initial_width_with_inset(shell: &CatalogUi, content_inset: i32) -> i32 {
    column_view_initial_width(shell, content_inset)
}
pub fn column_view_initial_width(shell: &CatalogUi, content_inset: i32) -> i32 {
    (shell.route_width)()
        .saturating_sub(content_inset.max(0))
        .saturating_sub(FITTED_TABLE_WIDTH_PADDING)
        .max(1)
}
