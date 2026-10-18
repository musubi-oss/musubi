fn main() {
    // sqlx::migrate!() embeds the directory at compile time; without this a new migration file
    // alone does not recompile the crate and the old set stays embedded.
    println!("cargo:rerun-if-changed=migrations");
}
