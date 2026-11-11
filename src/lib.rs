//! Dockerfile parser plugin — full-parse mode.
//!
//! Handles `Dockerfile` and `*.dockerfile` files.
//! Uses tree-sitter-dockerfile directly (no Python grammar package needed).

use intentumdiff_plugin_sdk::tree::{SemanticNode, SemanticNodeBuilder};
use intentumdiff_plugin_sdk::ts_convert::{convert_ts_direct, TsDirectHooks};

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
    // Exec-form arguments (CMD/ENTRYPOINT/RUN/SHELL/VOLUME/ADD/COPY ["..."]). Without
    // this the array projected to no children and an argument edit hashed style-only
    // (#5).
    "json_string",
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

/// Drop carriage returns that are part of a physical line terminator, as Docker's own
/// line scanner does (moby/buildkit `scanLines`/`dropCR`): a CR immediately before LF,
/// or a final CR at end of input. A lone CR anywhere else is content and is kept.
///
/// The grammar treats such a CR as command text (`shell_fragment` "echo hello\r") and
/// does not recognise `\` + CRLF as a line continuation, so an LF -> CRLF conversion
/// surfaced as a semantic RUN change (#4). Every dropped CR is the last byte of its row,
/// so the (row, UTF-8 byte column) positions of the parsed copy are exact positions in
/// the original source; a span merely ends before the terminator instead of inside it.
fn normalize_line_terminators(source: &str) -> std::borrow::Cow<'_, str> {
    if !source.contains('\r') {
        return std::borrow::Cow::Borrowed(source);
    }
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(source.len());
    let mut start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\r' && (i + 1 == bytes.len() || bytes[i + 1] == b'\n') {
            out.push_str(&source[start..i]);
            start = i + 1;
        }
    }
    out.push_str(&source[start..]);
    std::borrow::Cow::Owned(out)
}

fn process_impl(source: &str) -> String {
    let source = normalize_line_terminators(source);
    let source: &str = &source;
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

    // -- helpers for the projection regressions --------------------------------------

    fn tree(source: &str) -> serde_json::Value {
        let out = process_impl(source);
        t::assert_valid_json(&out, "process");
        serde_json::from_str(&out).unwrap()
    }

    /// Pre-order (node_type, label) projection: what host-side hashing and matching see.
    fn projection(source: &str) -> Vec<(String, String)> {
        fn walk(v: &serde_json::Value, out: &mut Vec<(String, String)>) {
            out.push((
                v["node_type"].as_str().unwrap().to_string(),
                v["label"].as_str().unwrap().to_string(),
            ));
            for c in v["children"].as_array().unwrap() {
                walk(c, out);
            }
        }
        let mut out = Vec::new();
        walk(&tree(source), &mut out);
        out
    }

    fn labels_of(source: &str, node_type: &str) -> Vec<String> {
        projection(source)
            .into_iter()
            .filter(|(k, _)| k == node_type)
            .map(|(_, l)| l)
            .collect()
    }

    // -- #5: exec-form arguments are semantic -----------------------------------------

    const PLAYGROUND_OLD: &str = "FROM node:18-alpine AS builder\nWORKDIR /app\nCOPY package.json ./\nRUN npm install --omit=dev\nENV APP_PORT=8080\nENV LOG_LEVEL=info\nEXPOSE 9091\nLABEL maintainer=\"platform-team\"\nHEALTHCHECK CMD curl --fail http://localhost:9091/health\nUSER svc_runner\nCMD [\"dist/index.js\"]\n";
    const PLAYGROUND_NEW: &str = "FROM node:18-alpine AS builder\nWORKDIR /app\nCOPY package.json ./\nRUN npm install --omit=dev\nENV APP_PORT=8080\nENV LOG_LEVEL=info\nEXPOSE 9091\nLABEL maintainer=\"platform-team\"\nHEALTHCHECK CMD curl --fail http://localhost:9091/health\nUSER svc_runner\nCMD [\"dist/worker.js\"]\n";

    #[test]
    fn exec_form_cmd_argument_change_is_projected() {
        assert_eq!(
            labels_of(PLAYGROUND_OLD, "json_string"),
            vec!["\"dist/index.js\"".to_string()]
        );
        assert_eq!(
            labels_of(PLAYGROUND_NEW, "json_string"),
            vec!["\"dist/worker.js\"".to_string()]
        );
        assert_ne!(projection(PLAYGROUND_OLD), projection(PLAYGROUND_NEW));
    }

    #[test]
    fn exec_form_keeps_every_argument_in_order() {
        let src = "CMD [\"node\", \"a \\\"q\\\" b\", \"h\u{e9}llo\u{2603}\"]\n";
        assert_eq!(
            labels_of(src, "json_string"),
            vec![
                "\"node\"".to_string(),
                "\"a \\\"q\\\" b\"".to_string(),
                "\"h\u{e9}llo\u{2603}\"".to_string(),
            ]
        );
        // Argument order is part of argv.
        assert_ne!(
            projection("CMD [\"a\", \"b\"]\n"),
            projection("CMD [\"b\", \"a\"]\n")
        );
        // Adding an argument is a change.
        assert_ne!(
            projection("CMD [\"a\"]\n"),
            projection("CMD [\"a\", \"b\"]\n")
        );
    }

    #[test]
    fn exec_form_whitespace_is_not_semantic() {
        assert_eq!(
            projection("CMD [\"node\",\"index.js\"]\n"),
            projection("CMD [ \"node\" ,   \"index.js\" ]\n")
        );
    }

    #[test]
    fn exec_form_entrypoint_and_run_share_the_projection() {
        for kw in ["ENTRYPOINT", "RUN", "SHELL", "VOLUME"] {
            let old = format!("{kw} [\"/bin/a\"]\n");
            let new = format!("{kw} [\"/bin/b\"]\n");
            assert_eq!(
                labels_of(&old, "json_string"),
                vec!["\"/bin/a\"".to_string()],
                "{kw}"
            );
            assert_ne!(projection(&old), projection(&new), "{kw}");
        }
    }

    // -- #4: CRLF terminators are layout, not command text ----------------------------

    /// LF and CRLF spellings of the same Dockerfile must produce identical trees
    /// (labels AND positions: every dropped CR is the last byte of its row).
    fn assert_crlf_equivalent(lf: &str) {
        let crlf = lf.replace('\n', "\r\n");
        assert_eq!(tree(lf), tree(&crlf), "LF vs CRLF tree differs for {lf:?}");
    }

    #[test]
    fn crlf_assessment_case_is_layout_only() {
        let old = "FROM scratch\nRUN echo hello\nCMD [\"/app\"]\n";
        let new = "FROM scratch\r\nRUN echo hello\r\nCMD [\"/app\"]\r\n";
        assert_eq!(tree(old), tree(new));
        assert_eq!(
            labels_of(new, "shell_fragment"),
            vec!["echo hello".to_string()]
        );
    }

    #[test]
    fn crlf_issue_reproduction_is_layout_only() {
        let old = "FROM node:22\nRUN npm install --omit=dev\n";
        let new = "FROM node:22\r\nRUN npm install --omit=dev\r\n";
        assert_eq!(tree(old), tree(new));
    }

    #[test]
    fn crlf_multiple_runs_continuations_and_instructions() {
        assert_crlf_equivalent(
            "FROM scratch\nRUN echo one\nRUN echo two \\\n  && ls -la\nUSER svc\nENV A=b C=\"d\"\nWORKDIR /app\nEXPOSE 80\nLABEL x=y\nARG Q=1\nCOPY a b\nHEALTHCHECK CMD curl x\nENTRYPOINT [\"x\", \"y\"]\n",
        );
    }

    #[test]
    fn crlf_continuation_is_not_swallowed() {
        // `\` + CRLF must continue the line like `\` + LF does; before the fix the
        // grammar stopped recognising every instruction after it.
        let crlf = "FROM scratch\r\nRUN a \\\r\n  b\r\nUSER svc\r\n";
        assert_eq!(
            labels_of(crlf, "user_instruction"),
            vec!["USER svc".to_string()]
        );
        assert!(labels_of(crlf, "shell_fragment")
            .iter()
            .all(|l| !l.contains('\r')));
    }

    #[test]
    fn crlf_mixed_endings_and_final_newline_variants() {
        let lf = "FROM scratch\nRUN echo hello\nRUN echo bye";
        assert_eq!(
            tree(lf),
            tree("FROM scratch\r\nRUN echo hello\nRUN echo bye")
        );
        assert_eq!(
            tree(lf),
            tree("FROM scratch\nRUN echo hello\r\nRUN echo bye")
        );
        // A trailing CR at end of input is a line terminator (moby dropCR at EOF).
        assert_eq!(
            tree(lf),
            tree("FROM scratch\nRUN echo hello\nRUN echo bye\r")
        );
        assert_crlf_equivalent("FROM scratch\nRUN echo hello\n");
    }

    #[test]
    fn crlf_real_command_change_stays_semantic() {
        assert_ne!(
            projection("FROM scratch\r\nRUN echo hello\r\n"),
            projection("FROM scratch\r\nRUN echo world\r\n")
        );
    }

    #[test]
    fn lone_cr_is_content_and_kept() {
        let src = "RUN printf 'a\rb'\n";
        assert!(labels_of(src, "shell_fragment")
            .iter()
            .any(|l| l.contains('\r')));
        assert_eq!(normalize_line_terminators("a\rb\r\nc\r"), "a\rb\nc");
        assert!(matches!(
            normalize_line_terminators("no cr\n"),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn crlf_positions_are_original_byte_columns() {
        let v = tree("FROM scratch\r\nRUN echo h\u{e9}\r\n");
        let run = &v["children"][1];
        assert_eq!(run["node_type"], "run_instruction");
        assert_eq!(run["position"]["start_line"], 1);
        // "RUN echo hé" is 12 UTF-8 bytes; the span ends before the CR.
        assert_eq!(run["position"]["end_col"], 12);
    }
}
