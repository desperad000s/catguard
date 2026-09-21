fn main() {
    // Compiles the icons into the exe. Does nothing for non-Windows targets.
    embed_resource::compile("assets/catguard.rc", embed_resource::NONE)
        .manifest_optional()
        .unwrap();
}
