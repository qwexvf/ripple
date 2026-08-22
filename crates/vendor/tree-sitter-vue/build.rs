// Compile the vendored Vue grammar: the generated parser (C) and the scanner (C++,
// which #includes the bundled HTML scanner). `src` is on the include path so the
// `tree_sitter/parser.h` and `./tree_sitter_html/...` includes resolve.

fn main() {
    let src = std::path::Path::new("src");
    cc::Build::new()
        .include(src)
        .file(src.join("parser.c"))
        .warnings(false)
        .compile("tree_sitter_vue_parser");

    cc::Build::new()
        .cpp(true)
        .include(src)
        .file(src.join("scanner.cc"))
        .warnings(false)
        .compile("tree_sitter_vue_scanner");

    println!("cargo:rerun-if-changed=src/parser.c");
    println!("cargo:rerun-if-changed=src/scanner.cc");
    println!("cargo:rerun-if-changed=src/tree_sitter_html/scanner.cc");
}
