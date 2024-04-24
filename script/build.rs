use sp1_helper::build_program;

fn main() {
    vergen::EmitBuilder::builder()
        .build_timestamp()
        .git_sha(true)
        .emit()
        .unwrap();

    build_program("../program")
}
