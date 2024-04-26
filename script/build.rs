use sp1_helper::build_program;

fn main() {
    // build_program("../program")
    vergen::EmitBuilder::builder()
        .build_timestamp()
        .git_sha(true)
        .emit()
        .unwrap();
}
