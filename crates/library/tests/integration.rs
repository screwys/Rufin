use library::LibraryResult;

#[expect(
    dead_code,
    reason = "Raw integration fixtures reuse production connection setup."
)]
#[path = "../src/schema.rs"]
mod production_schema;

mod activity;
mod entities;
mod laws;
mod playlists;
mod products;
mod queue;
mod scan;
mod schema;
mod services;
mod support;
