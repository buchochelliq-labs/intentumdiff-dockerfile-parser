//! Dockerfile parser plugin — full-parse mode.
//!
//! Handles `Dockerfile` and `*.dockerfile` files.
//! Uses tree-sitter-dockerfile directly (no Python grammar package needed).

use intentumdiff_plugin_sdk::ts_convert::{convert_ts_direct, TsDirectHooks};
use intentumdiff_plugin_sdk::tree::{SemanticNode, SemanticNodeBuilder};

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
struct DockerfileParser;

const TRIVIA: &[&str] = &["comment", "whitespace", "line_continuation"];

const SEMANTIC_TYPES: &[&str] = &[
    // Root
    "source_file",
    // Instructions — one node type per Dockerfile instruction
    "from_instruction",
    "run_instruction",
    "cmd_instruction",
    "label_instruction",
    "expose_instruction",
    "env_instruction",
    "add_instruction",
    "copy_instruction",
    "entrypoint_instruction",
    "volume_instruction",
    "user_instruction",
    "workdir_instruction",
    "arg_instruction",
    "onbuild_instruction",
    "stopsignal_instruction",
    "healthcheck_instruction",
    "shell_instruction",
    // Values
    "image_name",
    "image_tag",
    "image_digest",
    "image_alias",
    "double_quoted_string",
    "single_quoted_string",
    "unquoted_string",
    "json_string_array",
    "path",
    "param",
    "env_pair",
    "label_pair",
    "port",
    "shell_command",
    "shell_fragment",
];

fn is_semantic(node_type: &str) -> bool {
    SEMANTIC_TYPES.contains(&node_type)
}

fn label_for_ts(node: tree_sitter::Node<'_>, source: &[u8]) -> String {
    let kind = node.kind();
    let txt = |n: tree_sitter::Node<'_>| n.utf8_text(source).unwrap_or("").to_string();
    if node.child_count() == 0 {
        return node.utf8_text(source).unwrap_or("").to_string();
    }
    // String containers label with their SOURCE TEXT (#46): with the generic kind-name
    // label a LABEL value edit ("platform-team" -> "platform-teamX") hashed style-only.
    if matches!(
        kind,
        "double_quoted_string" | "single_quoted_string" | "json_string" | "unquoted_string"
    ) {
        let text = txt(node);
        if !text.trim().is_empty() {
            return text.chars().take(120).collect();
        }
    }
    match kind {
        "from_instruction" => {
            for i in 0..node.child_count() {
                let c = node.child(i).unwrap();
                if c.kind() == "image_name" || c.kind() == "image_spec" {
                    return txt(c);
                }
            }
            return "FROM".to_string();
        }
        "run_instruction" => return "RUN".to_string(),
        "cmd_instruction" => return "CMD".to_string(),
        "label_instruction" => return "LABEL".to_string(),
        "expose_instruction" => {
            for i in 0..node.child_count() {
                let c = node.child(i).unwrap();
                if c.kind() == "port" {
                    return format!("EXPOSE {}", txt(c));
                }
            }
            return "EXPOSE".to_string();
        }
        "env_instruction" => {
            for i in 0..node.child_count() {
                let c = node.child(i).unwrap();
                if c.kind() == "env_pair" {
                    if let Some(key) = c.child(0) {
                        return format!("ENV {}", txt(key));
                    }
                }
            }
            return "ENV".to_string();
        }
        "add_instruction" => return "ADD".to_string(),
        "copy_instruction" => {
            for i in 0..node.child_count() {
                let c = node.child(i).unwrap();
                if c.kind() == "path" {
                    return format!("COPY {}", txt(c));
                }
            }
            return "COPY".to_string();
        }
        "entrypoint_instruction" => return "ENTRYPOINT".to_string(),
        "volume_instruction" => return "VOLUME".to_string(),
        "user_instruction" => {
            for i in 0..node.child_count() {
                let c = node.child(i).unwrap();
                if c.kind() == "unquoted_string" || c.kind() == "user_name" {
                    return format!("USER {}", txt(c));
                }
            }
            return "USER".to_string();
        }
        "workdir_instruction" => {
            for i in 0..node.child_count() {
                let c = node.child(i).unwrap();
                if c.kind() == "path" || c.kind() == "unquoted_string" {
                    return format!("WORKDIR {}", txt(c));
                }
            }
            return "WORKDIR".to_string();
        }
        "arg_instruction" => {
            for i in 0..node.child_count() {
                let c = node.child(i).unwrap();
                if c.kind() == "unquoted_string" || c.kind() == "arg_name" {
                    return format!("ARG {}", txt(c));
                }
            }
            return "ARG".to_string();
        }
        "onbuild_instruction" => return "ONBUILD".to_string(),
        "healthcheck_instruction" => return "HEALTHCHECK".to_string(),
        "shell_instruction" => return "SHELL".to_string(),
        "stopsignal_instruction" => return "STOPSIGNAL".to_string(),
        _ => {}
    }
    kind.to_string()
}

fn convert_ts(node: tree_sitter::Node<'_>, source: &[u8], id_prefix: &str) -> Option<SemanticNode> {
    convert_ts_direct(
        node,
        source,
        id_prefix,
        None,
        &TsDirectHooks {
            is_trivia: &|kind| TRIVIA.contains(&kind),
            class_label: &|_, _| None,
            keep_childless: &|n| is_semantic(n.kind()),
            unwrap_single: &|_, _| false,
            label: &|n, s| label_for_ts(n, s),
            is_method_like: &|_| false,
        },
    )
}

fn process_impl(source: &str) -> String {
    let mut parser = tree_sitter::Parser::new();
    let lang = tree_sitter_dockerfile::LANGUAGE.into();
    if parser.set_language(&lang).is_err() {
        return r#"{"error":"Failed to load grammar"}"#.to_string();
    }
    let tree = match parser.parse(source, None) {
        Some(t) => t,
        None => return r#"{"error":"Parse failed"}"#.to_string(),
    };
    let root = tree.root_node();
    match convert_ts(root, source.as_bytes(), "0") {
        Some(n) => serde_json::to_string(&n).unwrap_or_else(|e| format!(r#"{{"error":"{}"}}"#, e)),
        None => r#"{"error":"Empty semantic tree"}"#.to_string(),
    }
}
impl Guest for DockerfileParser {
    fn get_parser_mode() -> ParserMode {
        ParserMode::FullParse
    }
    fn grammar_id() -> String {
        "dockerfile".to_string()
    }
    fn detect_language(filename: String, _content: String) -> String {
        let base = std::path::Path::new(&filename)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_lowercase();
        if base == "dockerfile"
            || base.starts_with("dockerfile.")
            || base.ends_with(".dockerfile")
            || base.ends_with(".containerfile")
            || base == "containerfile"
        {
            return "dockerfile".to_string();
        }
        String::new()
    }
    fn preprocess_source(source: String) -> String {
        source
    }
    fn example(_language: String) -> ExamplePair {
        ExamplePair {
            old: "FROM node:18\nWORKDIR /app\nCOPY . .\nRUN npm install\nCMD [\"node\", \"index.js\"]\n".to_string(),
            new: "FROM node:18-alpine AS builder\nWORKDIR /app\nCOPY package*.json ./\nRUN npm ci --only=production\n\nFROM node:18-alpine\nWORKDIR /app\nCOPY --from=builder /app/node_modules ./node_modules\nCOPY . .\nEXPOSE 3000\nCMD [\"node\", \"index.js\"]\n".to_string(),
        }
    }
    fn process(input: String, _language: String, _filename: String) -> String {
        process_impl(&input)
    }
    fn trivia_node_types() -> Vec<String> {
        TRIVIA.iter().map(|s| s.to_string()).collect()
    }
    fn language_ids() -> Vec<String> {
        vec!["dockerfile".to_string()]
    }
    fn language_info() -> Vec<LanguageInfoRecord> {
        language_info_for(Self::language_ids())
    }
    fn priority() -> i32 {
        0
    }
}

export!(DockerfileParser);

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
        assert!(!DockerfileParser::grammar_id().is_empty());
    }

    #[test]
    fn language_ids_contain_grammar_id() {
        let gid = DockerfileParser::grammar_id();
        let ids = DockerfileParser::language_ids();
        assert!(
            ids.contains(&gid),
            "language_ids {:?} must contain {:?}",
            ids,
            gid
        );
    }

    #[test]
    fn detect_language_known_ext() {
        let r = DockerfileParser::detect_language("Dockerfile".to_string(), "".to_string());
        assert_eq!(r.as_str(), "dockerfile");
    }

    #[test]
    fn detect_language_unknown_ext() {
        let r = DockerfileParser::detect_language(
            "test.xyz_notareal_ext_9z8y".to_string(),
            "".to_string(),
        );
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
