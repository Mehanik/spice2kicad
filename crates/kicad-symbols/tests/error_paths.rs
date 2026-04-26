//! Error paths: missing file, malformed sexp, wrong root, bad pin angle.

use std::io::Write;

use kicad_symbols::{Library, LoadError};

fn write_temp(name: &str, contents: &str) -> std::path::PathBuf {
    let mut path = std::env::temp_dir();
    let pid = std::process::id();
    path.push(format!("kicad-symbols-test-{pid}-{name}"));
    let mut f = std::fs::File::create(&path).expect("create temp file");
    f.write_all(contents.as_bytes()).expect("write temp file");
    path
}

#[test]
fn missing_file_is_io_error() {
    let path = std::env::temp_dir().join("kicad-symbols-definitely-not-a-file.kicad_sym");
    let _ = std::fs::remove_file(&path);
    match Library::from_file(&path) {
        Err(LoadError::Io { .. }) => {}
        other => panic!("expected LoadError::Io, got {other:?}"),
    }
}

#[test]
fn malformed_sexp_is_parse_error() {
    let p = write_temp("bad.kicad_sym", "(kicad_symbol_lib (symbol \"R\" ");
    let res = Library::from_file(&p);
    let _ = std::fs::remove_file(&p);
    match res {
        Err(LoadError::Parse { .. }) => {}
        other => panic!("expected LoadError::Parse, got {other:?}"),
    }
}

#[test]
fn wrong_root_is_structure_error() {
    let p = write_temp("wrong_root.kicad_sym", "(not_a_symbol_lib)");
    let res = Library::from_file(&p);
    let _ = std::fs::remove_file(&p);
    match res {
        Err(LoadError::Structure { .. }) => {}
        other => panic!("expected LoadError::Structure, got {other:?}"),
    }
}

#[test]
fn non_90_degree_pin_angle_is_structure_error() {
    let p = write_temp(
        "bad_angle.kicad_sym",
        r#"(kicad_symbol_lib
            (symbol "Bogus"
              (symbol "Bogus_1_1"
                (pin passive line
                  (at 0 0 45)
                  (length 1.27)
                  (name "~")
                  (number "1")))))"#,
    );
    let res = Library::from_file(&p);
    let _ = std::fs::remove_file(&p);
    match res {
        Err(LoadError::Structure { message, .. }) => {
            assert!(
                message.contains("multiple of 90") || message.contains("integer degree"),
                "unexpected message: {message}"
            );
        }
        other => panic!("expected LoadError::Structure, got {other:?}"),
    }
}

#[test]
fn pin_missing_number_is_structure_error() {
    let p = write_temp(
        "no_number.kicad_sym",
        r#"(kicad_symbol_lib
            (symbol "Bogus"
              (symbol "Bogus_1_1"
                (pin passive line
                  (at 0 0 0)
                  (length 1.27)
                  (name "~")))))"#,
    );
    let res = Library::from_file(&p);
    let _ = std::fs::remove_file(&p);
    match res {
        Err(LoadError::Structure { .. }) => {}
        other => panic!("expected LoadError::Structure, got {other:?}"),
    }
}
