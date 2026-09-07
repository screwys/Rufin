pub const ROUTE_PLACEHOLDER_RESOURCE: &str = "/io/github/screwys/Rufin/ui/routes/placeholder.ui";
pub const INTERFACE_RESOURCE_PATHS: &[&str] = &[
    SEARCH_ROUTE_RESOURCE,
    LIBRARY_TOOLBAR_RESOURCE,
    LIBRARY_PAGE_RESOURCE,
    LIBRARY_CONFIG_RESOURCE,
    HOME_SECTION_HEADER_RESOURCE,
    FOLDERS_RESOURCE,
    DETAIL_SHOWCASE_RESOURCE,
    COLLECTION_GRID_COVER_RESOURCE,
    COLLECTION_GRID_CARD_RESOURCE,
    ROUTE_PLACEHOLDER_RESOURCE,
];
pub const SEARCH_ROUTE_RESOURCE: &str = "/io/github/screwys/Rufin/ui/routes/search.ui";
pub const LIBRARY_TOOLBAR_RESOURCE: &str = "/io/github/screwys/Rufin/ui/routes/library_toolbar.ui";
pub const LIBRARY_PAGE_RESOURCE: &str = "/io/github/screwys/Rufin/ui/routes/library_page.ui";
pub const LIBRARY_CONFIG_RESOURCE: &str = "/io/github/screwys/Rufin/ui/routes/library_config.ui";
pub const HOME_SECTION_HEADER_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/routes/home_section_header.ui";
pub const FOLDERS_RESOURCE: &str = "/io/github/screwys/Rufin/ui/routes/folders.ui";
pub const DETAIL_SHOWCASE_RESOURCE: &str = "/io/github/screwys/Rufin/ui/routes/detail_showcase.ui";
pub const COLLECTION_GRID_COVER_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/routes/collection_grid_cover.ui";
pub const COLLECTION_GRID_CARD_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/routes/collection_grid_card.ui";

#[cfg(test)]
mod resource_tests {
    #[test]
    fn owned_interface_resources_are_compiled() {
        crate::register_resources().expect("resource registration");
        for path in super::INTERFACE_RESOURCE_PATHS {
            gtk::gio::resources_lookup_data(path, gtk::gio::ResourceLookupFlags::NONE)
                .expect("compiled interface resource");
        }
    }
}
