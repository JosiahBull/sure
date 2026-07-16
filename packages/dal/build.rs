// Force a rebuild (so the embedded `sqlx::migrate!` set is refreshed) whenever a
// migration is added or changed. Without this, a newly-added .sql file can be missed
// until the crate is otherwise recompiled.
fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
