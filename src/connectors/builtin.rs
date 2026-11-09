//! Definitions of the built-in tools. Handlers are injected so evals can mock them.

use crate::tools::{Handler, Tool, params, prop};
use serde_json::json;

/// One async handler per built-in tool.
pub struct Handlers {
    pub insert_text: Handler,
    pub insert_image: Handler,
    pub read_clipboard: Handler,
    pub list_windows: Handler,
    pub search_notes: Handler,
    pub read_note: Handler,
    pub recent_emails: Handler,
    pub delegate: Handler,
}

/// `delegate_agents`: installed coding harnesses as (name, description), preferred first.
pub fn builtin_tools(h: Handlers, delegate_agents: &[(String, String)]) -> Vec<Tool> {
    let mut tools = vec![
        Tool::new(
            "insert_text",
            "Insert text at the user's text cursor in the focused app (as if pasted). \
             Use for drafting, replying, filling in, rewriting.",
            params(&["text"], &[("text", prop("string", "The exact text to insert"))]),
            h.insert_text,
            "Inserting",
        ),
        Tool::new(
            "insert_image",
            "Paste an image file at the user's cursor.",
            params(&["path"], &[("path", prop("string", "Absolute path to a PNG/JPEG file"))]),
            h.insert_image,
            "Inserting image",
        ),
        Tool::new(
            "read_clipboard",
            "Read the current clipboard text.",
            params(&[], &[]),
            h.read_clipboard,
            "Reading clipboard",
        ),
        Tool::new(
            "list_windows",
            "List the user's open windows/apps with titles.",
            params(&[], &[]),
            h.list_windows,
            "Looking at open apps",
        ),
        Tool::new(
            "search_notes",
            "Search the user's notes (Markdown/text folders). Returns paths, titles and snippets.",
            params(&["query"], &[("query", prop("string", "Keywords")), ("limit", prop("integer", ""))]),
            h.search_notes,
            "Searching notes",
        ),
        Tool::new(
            "read_note",
            "Read a note's full text by path (from search_notes).",
            params(&["path"], &[("path", prop("string", ""))]),
            h.read_note,
            "Reading note",
        ),
        Tool::new(
            "recent_emails",
            "Recent inbox email from the local Thunderbird client, newest first.",
            params(
                &[],
                &[
                    ("limit", prop("integer", "How many (default 10)")),
                    ("query", prop("string", "Optional filter text")),
                    ("include_body", prop("boolean", "Return full bodies")),
                ],
            ),
            h.recent_emails,
            "Checking email",
        ),
    ];
    if !delegate_agents.is_empty() {
        let names: Vec<&str> = delegate_agents.iter().map(|(n, _)| n.as_str()).collect();
        let listing: Vec<String> =
            delegate_agents.iter().map(|(n, d)| if d.is_empty() { n.clone() } else { format!("{n} ({d})") }).collect();
        tools.push(Tool::new(
            "delegate",
            &format!(
                "Hand a difficult task (coding, debugging, multi-step research) to a coding agent. \
                 Available: {}. Use {} unless the user asks for another. When Claude Code or Codex is \
                 installed, it plans the work for a local agent and reviews the result automatically. \
                 Returns the final output.",
                listing.join("; "),
                names[0]
            ),
            params(
                &["agent", "task"],
                &[
                    ("agent", json!({"type": "string", "enum": names})),
                    ("task", prop("string", "Complete, self-contained task description")),
                    ("cwd", prop("string", "Working directory (optional)")),
                ],
            ),
            h.delegate,
            "Delegating",
        ));
    }
    tools
}
