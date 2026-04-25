//! Minimal S-expression writer for KiCad files.

use std::fmt::Write;

#[derive(Debug, Clone)]
pub enum Sexpr {
    Atom(String),
    QString(String),
    List(Vec<Sexpr>),
}

impl Sexpr {
    pub fn write(&self, out: &mut String, indent: usize) {
        match self {
            Sexpr::Atom(s) => out.push_str(s),
            Sexpr::QString(s) => {
                out.push('"');
                for c in s.chars() {
                    match c {
                        '"' => out.push_str("\\\""),
                        '\\' => out.push_str("\\\\"),
                        _ => out.push(c),
                    }
                }
                out.push('"');
            }
            Sexpr::List(items) => {
                out.push('(');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(' ');
                    }
                    item.write(out, indent + 1);
                }
                out.push(')');
            }
        }
    }

    pub fn to_pretty(&self) -> String {
        let mut s = String::new();
        self.write(&mut s, 0);
        let _ = writeln!(&mut s);
        s
    }
}
