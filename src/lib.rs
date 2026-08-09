//! C# parser plugin - full-parse mode.
//!
//! Handles `.cs` files. Parses source with tree-sitter-c-sharp directly.

use intentumdiff_plugin_sdk::{
    cst::CstNode,
    hash::structural_hash_with_memo,
    tree::{SemanticNode, SemanticNodeBuilder},
};

wit_bindgen::generate!({
    path: "wit/plugin.wit",
    world: "parser-plugin",
});

use crate::exports::intentdiff::plugin::parser::ExamplePair;
use crate::exports::intentdiff::plugin::parser::Guest;
use crate::exports::intentdiff::plugin::parser::LanguageInfoRecord;
use crate::exports::intentdiff::plugin::parser::ParserMode;

const PLUGIN_METADATA: &str = include_str!("../plugin_metadata.info");

fn language_info_for(ids: Vec<String>) -> Vec<LanguageInfoRecord> {
    let metadata = intentumdiff_plugin_sdk::metadata::parse_plugin_metadata(PLUGIN_METADATA);
    ids.into_iter()
        .map(|language_id| {
            let info = metadata.language_or_default(&language_id);
            LanguageInfoRecord {
                language_id: info.language_id,
                language_name: info.language_name,
                language_short_name: info.language_short_name,
                monaco_language: info.monaco_language,
                default_filename: info.default_filename,
                language_file_extensions: info.language_file_extensions,
                author: metadata.author().to_string(),
                plugin_version: metadata.plugin_version().to_string(),
                last_updated: metadata.last_updated().to_string(),
            }
        })
        .collect()
}
struct CSharpParser;

const TRIVIA: &[&str] = &[
    "comment",
    "line_comment",
    "block_comment",
    "whitespace",
    "single_line_comment",
    "multiline_comment",
];

const SEMANTIC_TYPES: &[&str] = &[
    // Root
    "compilation_unit",
    // Namespace / using
    "namespace_declaration",
    "file_scoped_namespace_declaration",
    "using_directive",
    // Type declarations
    "class_declaration",
    "interface_declaration",
    "struct_declaration",
    "record_declaration",
    "record_struct_declaration",
    "enum_declaration",
    "delegate_declaration",
    // Members
    "method_declaration",
    "constructor_declaration",
    "destructor_declaration",
    "property_declaration",
    "indexer_declaration",
    "event_declaration",
    "field_declaration",
    "operator_declaration",
    "conversion_operator_declaration",
    "enum_member_declaration",
    // Statements
    "local_declaration_statement",
    "expression_statement",
    "return_statement",
    "throw_statement",
    "if_statement",
    "for_statement",
    "foreach_statement",
    "while_statement",
    "do_statement",
    "try_statement",
    "catch_clause",
    "finally_clause",
    "switch_statement",
    "switch_expression",
    "lock_statement",
    "using_statement",
    "yield_statement",
    "break_statement",
    "continue_statement",
    "goto_statement",
    "labeled_statement",
    "checked_statement",
    "unchecked_statement",
    "unsafe_statement",
    "fixed_statement",
    // Expressions
    "assignment_expression",
    "invocation_expression",
    "object_creation_expression",
    "lambda_expression",
    "anonymous_method_expression",
    "await_expression",
    "query_expression",
    // Types / identifiers
    "identifier",
    "qualified_name",
    // Type nodes — must be semantic so ``var`` (implicit_type) and explicit
    // types (predefined_type like ``int``, generic_name like ``List<T>``)
    // produce distinct semantic trees. Without these, ``var count = ...`` and
    // ``int count = ...`` produce shape-identical trees because the type
    // token gets dropped (see ``test_csharp_var_to_explicit_type_preserves_initializer``
    // and ``docs/NOISE_SUPPRESSION_RETUNE.md`` Phase D).
    "implicit_type",
    "predefined_type",
    "generic_name",
    "array_type",
    "nullable_type",
    "pointer_type",
    "ref_type",
    "tuple_type",
    "string_literal",
    "integer_literal",
    "boolean_literal",
    "null_literal",
    // Attributes
    "attribute",
    "attribute_list",
];

fn is_semantic(node_type: &str) -> bool {
    SEMANTIC_TYPES.contains(&node_type)
}

fn label_for(node: &CstNode) -> String {
    if node.is_leaf() {
        return node.text_or_empty().to_string();
    }
    // Literal containers label with their captured source text (SDK-shared, issue #47).
    if let Some(label) = intentumdiff_plugin_sdk::ts_convert::literal_label(node) {
        return label;
    }
    match node.node_type.as_str() {
        // Type nodes: use the source text so ``var`` and ``int`` produce
        // distinct labels even though both are leaf-like wrappers.
        "implicit_type" | "predefined_type" | "generic_name" | "array_type"
        | "nullable_type" | "pointer_type" | "ref_type" | "tuple_type" => {
            return node.text_or_empty().to_string();
        }
        "class_declaration"
        | "interface_declaration"
        | "struct_declaration"
        | "record_declaration"
        | "record_struct_declaration"
        | "enum_declaration"
        | "delegate_declaration"
        | "namespace_declaration"
        | "file_scoped_namespace_declaration" => {
            for child in &node.children {
                if child.node_type == "identifier" || child.node_type == "qualified_name" {
                    return child.text_or_empty().to_string();
                }
            }
        }
        "method_declaration"
        | "constructor_declaration"
        | "destructor_declaration"
        | "property_declaration"
        | "event_declaration" => {
            for child in &node.children {
                if child.node_type == "identifier" {
                    return child.text_or_empty().to_string();
                }
            }
        }
        "attribute" => {
            for child in &node.children {
                if child.node_type == "identifier" || child.node_type == "qualified_name" {
                    return child.text_or_empty().to_string();
                }
            }
        }
        "using_directive" => {
            for child in &node.children {
                if child.node_type == "qualified_name" || child.node_type == "identifier" {
                    return child.text_or_empty().to_string();
                }
            }
        }
        _ => {}
    }
    for child in &node.children {
        if child.node_type == "identifier" {
            return child.text_or_empty().to_string();
        }
    }
    node.node_type.clone()
}

fn is_class_like(node_type: &str) -> bool {
    matches!(
        node_type,
        "class_declaration"
            | "interface_declaration"
            | "struct_declaration"
            | "record_declaration"
            | "record_struct_declaration"
            | "enum_declaration"
    )
}

fn is_method_like(node_type: &str) -> bool {
    matches!(
        node_type,
        "method_declaration"
            | "constructor_declaration"
            | "destructor_declaration"
            | "property_declaration"
            | "operator_declaration"
    )
}

fn convert(
    node: &CstNode,
    id_prefix: &str,
    parent_class: Option<&str>,
    memo: &mut std::collections::HashMap<usize, String>,
) -> Option<SemanticNode> {
    convert_semantic_classed(
        node,
        id_prefix,
        parent_class,
        memo,
        &|_| false,
        &is_semantic,
        &is_class_like,
        &is_method_like,
        &label_for,
    )
}



use intentumdiff_plugin_sdk::ts_convert::{convert_semantic_classed, node_to_cst};

fn parse_source(source: &str) -> Result<CstNode, String> {
    let mut parser = tree_sitter::Parser::new();
    let lang = tree_sitter_c_sharp::LANGUAGE.into();
    parser
        .set_language(&lang)
        .map_err(|_| "Failed to load C# grammar".to_string())?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| "Parse failed".to_string())?;
    Ok(node_to_cst(tree.root_node(), source.as_bytes()))
}

fn process_impl(source: &str) -> String {
    let root: CstNode = match parse_source(source) {
        Ok(n) => n,
        Err(e) => return format!(r#"{{"error":"{}"}}"#, e),
    };
    let mut memo: std::collections::HashMap<usize, String> = std::collections::HashMap::new();
    let sem = match convert(&root, "0", None, &mut memo) {
        Some(n) => n,
        None => return r#"{"error":"Empty semantic tree"}"#.to_string(),
    };
    match serde_json::to_string(&sem) {
        Ok(s) => s,
        Err(e) => format!(r#"{{"error":"Serialisation error: {}"}}"#, e),
    }
}

impl Guest for CSharpParser {
    fn get_parser_mode() -> ParserMode {
        ParserMode::FullParse
    }
    fn grammar_id() -> String {
        "csharp".to_string()
    }
    fn detect_language(filename: String, _content: String) -> String {
        if filename.ends_with(".cs") {
            "csharp".to_string()
        } else {
            String::new()
        }
    }
    fn preprocess_source(source: String) -> String {
        source
    }
    fn example(_language: String) -> ExamplePair {
        ExamplePair {
            old: "using System;\n\nclass Greeter {\n    public void Greet(string name) {\n        Console.WriteLine(\"Hello, \" + name);\n    }\n}\n".to_string(),
            new: "using System;\n\nclass Greeter {\n    public void Greet(string name) {\n        Console.WriteLine($\"Hello, {name}!\");\n    }\n\n    public void GreetAll(string[] names) {\n        foreach (var name in names) Greet(name);\n    }\n}\n".to_string(),
        }
    }
    fn process(input: String, _language: String, _filename: String) -> String {
        process_impl(&input)
    }
    fn trivia_node_types() -> Vec<String> {
        TRIVIA.iter().map(|s| s.to_string()).collect()
    }
    fn language_ids() -> Vec<String> {
        vec!["csharp".to_string()]
    }
    fn language_info() -> Vec<LanguageInfoRecord> {
        language_info_for(Self::language_ids())
    }
    fn priority() -> i32 {
        0
    }
}

export!(CSharpParser);

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exports::intentdiff::plugin::parser::Guest;
    use intentumdiff_plugin_sdk::testing as t;

    #[test]
    fn grammar_id_nonempty() {
        assert!(!CSharpParser::grammar_id().is_empty());
    }

    #[test]
    fn language_ids_contain_grammar_id() {
        let gid = CSharpParser::grammar_id();
        let ids = CSharpParser::language_ids();
        assert!(
            ids.contains(&gid),
            "language_ids {:?} must contain {:?}",
            ids,
            gid
        );
    }

    #[test]
    fn detect_language_known_ext() {
        let r = CSharpParser::detect_language("test.cs".to_string(), "".to_string());
        assert_eq!(r.as_str(), "csharp");
    }

    #[test]
    fn detect_language_unknown_ext() {
        let r =
            CSharpParser::detect_language("test.xyz_notareal_ext_9z8y".to_string(), "".to_string());
        assert_eq!(r.as_str(), "");
    }

    #[test]
    fn process_impl_empty_returns_valid_json() {
        let out = process_impl("");
        t::assert_valid_json(&out, "process(empty)");
    }

    #[test]
    fn process_impl_whitespace_returns_valid_json() {
        let out = process_impl("   \n  ");
        t::assert_valid_json(&out, "process(whitespace)");
    }
}
