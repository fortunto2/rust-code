//! Extract symbols (functions, structs, impls, traits) from source files via tree-sitter.

use std::path::Path;

/// Kind of code symbol.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Function,
    Struct,
    Enum,
    Trait,
    Impl,
    Const,
    Static,
    TypeAlias,
    Mod,
    Class,
    Method,
}

/// A code symbol extracted from a source file.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub line: usize,
    pub public: bool,
}

/// Extract symbols from a file. Returns empty vec for unsupported languages.
pub fn extract_symbols(path: &Path, source: &str) -> Vec<Symbol> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    match ext {
        #[cfg(feature = "rust")]
        "rs" => extract_rust(source),
        #[cfg(feature = "python")]
        "py" => extract_python(source),
        #[cfg(feature = "typescript")]
        "ts" | "tsx" => extract_typescript(source),
        _ => vec![],
    }
}

// ---------------------------------------------------------------------------
// Rust
// ---------------------------------------------------------------------------

#[cfg(feature = "rust")]
fn extract_rust(source: &str) -> Vec<Symbol> {
    let mut parser = tree_sitter::Parser::new();
    let lang = tree_sitter_rust::LANGUAGE;
    parser.set_language(&lang.into()).expect("tree-sitter-rust");

    let Some(tree) = parser.parse(source, None) else {
        return vec![];
    };

    let mut symbols = Vec::new();
    collect_rust_symbols(&tree.root_node(), source.as_bytes(), &mut symbols, false);
    symbols
}

#[cfg(feature = "rust")]
fn collect_rust_symbols(
    node: &tree_sitter::Node,
    src: &[u8],
    symbols: &mut Vec<Symbol>,
    parent_public: bool,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let kind_str = child.kind();

        // Check for pub visibility
        let is_pub = parent_public || has_pub_child(&child);

        match kind_str {
            "function_item" => {
                if let Some(name) = child_by_field(&child, "name", src) {
                    symbols.push(Symbol {
                        name,
                        kind: SymbolKind::Function,
                        line: child.start_position().row + 1,
                        public: is_pub,
                    });
                }
            }
            "struct_item" => {
                if let Some(name) = child_by_field(&child, "name", src) {
                    symbols.push(Symbol {
                        name,
                        kind: SymbolKind::Struct,
                        line: child.start_position().row + 1,
                        public: is_pub,
                    });
                }
            }
            "enum_item" => {
                if let Some(name) = child_by_field(&child, "name", src) {
                    symbols.push(Symbol {
                        name,
                        kind: SymbolKind::Enum,
                        line: child.start_position().row + 1,
                        public: is_pub,
                    });
                }
            }
            "trait_item" => {
                if let Some(name) = child_by_field(&child, "name", src) {
                    symbols.push(Symbol {
                        name,
                        kind: SymbolKind::Trait,
                        line: child.start_position().row + 1,
                        public: is_pub,
                    });
                }
            }
            "impl_item" => {
                if let Some(name) = child_by_field(&child, "type", src) {
                    symbols.push(Symbol {
                        name,
                        kind: SymbolKind::Impl,
                        line: child.start_position().row + 1,
                        public: false,
                    });
                    // Collect methods inside impl block
                    collect_rust_symbols(&child, src, symbols, is_pub);
                }
            }
            "const_item" => {
                if let Some(name) = child_by_field(&child, "name", src) {
                    symbols.push(Symbol {
                        name,
                        kind: SymbolKind::Const,
                        line: child.start_position().row + 1,
                        public: is_pub,
                    });
                }
            }
            "static_item" => {
                if let Some(name) = child_by_field(&child, "name", src) {
                    symbols.push(Symbol {
                        name,
                        kind: SymbolKind::Static,
                        line: child.start_position().row + 1,
                        public: is_pub,
                    });
                }
            }
            "type_item" => {
                if let Some(name) = child_by_field(&child, "name", src) {
                    symbols.push(Symbol {
                        name,
                        kind: SymbolKind::TypeAlias,
                        line: child.start_position().row + 1,
                        public: is_pub,
                    });
                }
            }
            "mod_item" => {
                if let Some(name) = child_by_field(&child, "name", src) {
                    symbols.push(Symbol {
                        name,
                        kind: SymbolKind::Mod,
                        line: child.start_position().row + 1,
                        public: is_pub,
                    });
                }
            }
            _ => {
                collect_rust_symbols(&child, src, symbols, false);
            }
        }
    }
}

#[cfg(feature = "rust")]
fn has_pub_child(node: &tree_sitter::Node) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "visibility_modifier" {
            return true;
        }
    }
    false
}

#[cfg(feature = "rust")]
fn child_by_field(node: &tree_sitter::Node, field: &str, src: &[u8]) -> Option<String> {
    let child = node.child_by_field_name(field)?;
    child.utf8_text(src).ok().map(|s| s.to_string())
}

// ---------------------------------------------------------------------------
// Python
// ---------------------------------------------------------------------------

#[cfg(feature = "python")]
fn extract_python(source: &str) -> Vec<Symbol> {
    let mut parser = tree_sitter::Parser::new();
    let lang = tree_sitter_python::LANGUAGE;
    parser
        .set_language(&lang.into())
        .expect("tree-sitter-python");

    let Some(tree) = parser.parse(source, None) else {
        return vec![];
    };

    let mut symbols = Vec::new();
    collect_python_symbols(&tree.root_node(), source.as_bytes(), &mut symbols);
    symbols
}

#[cfg(feature = "python")]
fn collect_python_symbols(node: &tree_sitter::Node, src: &[u8], symbols: &mut Vec<Symbol>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "function_definition" => {
                if let Some(name) = child_by_field_py(&child, "name", src) {
                    let is_method = node.kind() == "block"
                        && node
                            .parent()
                            .map_or(false, |p| p.kind() == "class_definition");
                    symbols.push(Symbol {
                        public: !name.starts_with('_'),
                        name,
                        kind: if is_method {
                            SymbolKind::Method
                        } else {
                            SymbolKind::Function
                        },
                        line: child.start_position().row + 1,
                    });
                }
            }
            "class_definition" => {
                if let Some(name) = child_by_field_py(&child, "name", src) {
                    symbols.push(Symbol {
                        public: !name.starts_with('_'),
                        name,
                        kind: SymbolKind::Class,
                        line: child.start_position().row + 1,
                    });
                    collect_python_symbols(&child, src, symbols);
                }
            }
            _ => {
                collect_python_symbols(&child, src, symbols);
            }
        }
    }
}

#[cfg(feature = "python")]
fn child_by_field_py(node: &tree_sitter::Node, field: &str, src: &[u8]) -> Option<String> {
    let child = node.child_by_field_name(field)?;
    child.utf8_text(src).ok().map(|s| s.to_string())
}

// ---------------------------------------------------------------------------
// TypeScript
// ---------------------------------------------------------------------------

#[cfg(feature = "typescript")]
fn extract_typescript(source: &str) -> Vec<Symbol> {
    let mut parser = tree_sitter::Parser::new();
    let lang = tree_sitter_typescript::LANGUAGE_TYPESCRIPT;
    parser
        .set_language(&lang.into())
        .expect("tree-sitter-typescript");

    let Some(tree) = parser.parse(source, None) else {
        return vec![];
    };

    let mut symbols = Vec::new();
    collect_ts_symbols(&tree.root_node(), source.as_bytes(), &mut symbols);
    symbols
}

#[cfg(feature = "typescript")]
fn collect_ts_symbols(node: &tree_sitter::Node, src: &[u8], symbols: &mut Vec<Symbol>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "function_declaration" => {
                if let Some(name) = child_by_field_ts(&child, "name", src) {
                    symbols.push(Symbol {
                        name,
                        kind: SymbolKind::Function,
                        line: child.start_position().row + 1,
                        public: has_export(&child),
                    });
                }
            }
            "class_declaration" => {
                if let Some(name) = child_by_field_ts(&child, "name", src) {
                    symbols.push(Symbol {
                        name,
                        kind: SymbolKind::Class,
                        line: child.start_position().row + 1,
                        public: has_export(&child),
                    });
                    collect_ts_symbols(&child, src, symbols);
                }
            }
            "interface_declaration" | "type_alias_declaration" => {
                if let Some(name) = child_by_field_ts(&child, "name", src) {
                    symbols.push(Symbol {
                        name,
                        kind: SymbolKind::TypeAlias,
                        line: child.start_position().row + 1,
                        public: has_export(&child),
                    });
                }
            }
            "enum_declaration" => {
                if let Some(name) = child_by_field_ts(&child, "name", src) {
                    symbols.push(Symbol {
                        name,
                        kind: SymbolKind::Enum,
                        line: child.start_position().row + 1,
                        public: has_export(&child),
                    });
                }
            }
            "export_statement" => {
                collect_ts_symbols(&child, src, symbols);
            }
            _ => {
                collect_ts_symbols(&child, src, symbols);
            }
        }
    }
}

#[cfg(feature = "typescript")]
fn has_export(node: &tree_sitter::Node) -> bool {
    node.parent()
        .map_or(false, |p| p.kind() == "export_statement")
}

#[cfg(feature = "typescript")]
fn child_by_field_ts(node: &tree_sitter::Node, field: &str, src: &[u8]) -> Option<String> {
    let child = node.child_by_field_name(field)?;
    child.utf8_text(src).ok().map(|s| s.to_string())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[cfg(feature = "rust")]
    #[test]
    fn extract_rust_symbols() {
        let src = r#"
pub struct Config {
    name: String,
}

pub enum Action {
    Run,
    Stop,
}

pub trait Agent {
    fn decide(&self);
}

impl Config {
    pub fn new() -> Self {
        Self { name: String::new() }
    }

    fn private_method(&self) {}
}

pub fn main() {}

const MAX: usize = 10;

pub type Result<T> = std::result::Result<T, Error>;
"#;
        let symbols = extract_symbols(Path::new("test.rs"), src);
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();

        assert!(names.contains(&"Config"), "missing struct Config");
        assert!(names.contains(&"Action"), "missing enum Action");
        assert!(names.contains(&"Agent"), "missing trait Agent");
        assert!(names.contains(&"main"), "missing fn main");
        assert!(names.contains(&"new"), "missing fn new");
        assert!(names.contains(&"MAX"), "missing const MAX");
        assert!(names.contains(&"Result"), "missing type alias Result");

        // Check pub detection
        let main_sym = symbols.iter().find(|s| s.name == "main").unwrap();
        assert!(main_sym.public);

        let priv_sym = symbols.iter().find(|s| s.name == "private_method").unwrap();
        assert!(!priv_sym.public);
    }

    #[cfg(feature = "python")]
    #[test]
    fn extract_python_symbols() {
        let src = r#"
class MyClass:
    def public_method(self):
        pass

    def _private_method(self):
        pass

def top_level_func():
    pass
"#;
        let symbols = extract_symbols(Path::new("test.py"), src);
        assert!(symbols.iter().any(|s| s.name == "MyClass"));
        assert!(symbols.iter().any(|s| s.name == "top_level_func"));
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "_private_method" && !s.public)
        );
    }
}
