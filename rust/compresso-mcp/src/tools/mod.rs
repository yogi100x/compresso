pub(crate) mod atlas_nav;
pub(crate) mod atlas_read;
pub(crate) mod common;

use compresso_store::Store;
use rmcp::model::{CallToolResult, JsonObject};

pub(crate) fn handles(name: &str) -> bool {
    atlas_nav::handles(name) || atlas_read::handles(name)
}

pub(crate) fn dispatch(store: &Store, name: &str, arguments: Option<JsonObject>) -> CallToolResult {
    if atlas_nav::handles(name) {
        atlas_nav::dispatch(store, name, arguments)
    } else {
        atlas_read::dispatch(store, name, arguments)
    }
}
