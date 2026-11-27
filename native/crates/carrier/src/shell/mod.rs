//! Explorer's "Open with Rainy" entries and the file types Rainy offers to open, kept in the current user's registry
//! (`shell-integration.ts` in 3.x). Nothing here needs administrator rights, and Windows never lets an application make
//! itself the default app: the user picks Rainy in "Open with" or in the Default apps settings. Both parts stay off, and
//! nothing is written, until the user turns them on.

mod catalog;
mod plan;
#[cfg(windows)]
mod win32;
#[cfg(test)]
mod tests;

pub use self::catalog::{
    DEFAULT_APPS, DEFAULT_APPS_LIST, FILE_TYPES, FileType, Language, REGISTERED_APPLICATION, RETIRED_EXTENSIONS,
    ShellIntegration, context_menu_label, extensions, launch_command, prog_id,
};
pub use self::plan::{
    Change, Memory, Operation, Registry, Status, Synced, apply, change_saved, current, existing_keys, memory,
    operations, preference, read_status, removable_keys, retire_operations, save, sync, sync_saved, unregister,
    unregister_saved,
};
#[cfg(windows)]
pub use self::win32::{CurrentUser, Redirected, open_default_apps};
