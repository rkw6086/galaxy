use std::path::PathBuf;

use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionResponse, FileChangeType, FileEvent,
    InsertTextFormat, Location as LspLocation, LocationLink, Position as LspPosition,
    Range as LspRange,
};

use crate::config::{lsp_uri_to_path, path_to_lsp_uri};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileLocation {
    pub path: PathBuf,
    pub location: Location,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub line: usize,
    pub column: usize,
}

impl Location {
    pub fn into_lsp(self) -> LspPosition {
        LspPosition {
            line: self.line as u32,
            character: self.column as u32,
        }
    }
}

impl From<LspLocation> for Location {
    fn from(location: LspLocation) -> Self {
        Self {
            line: location.range.start.line as usize,
            column: location.range.start.character as usize,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Range {
    pub start: Location,
    pub end: Location,
}

impl Range {
    fn into_lsp(self) -> LspRange {
        LspRange {
            start: self.start.into_lsp(),
            end: self.end.into_lsp(),
        }
    }
}

impl From<LspRange> for Range {
    fn from(range: LspRange) -> Self {
        Self {
            start: Location {
                line: range.start.line as usize,
                column: range.start.character as usize,
            },
            end: Location {
                line: range.end.line as usize,
                column: range.end.character as usize,
            },
        }
    }
}

#[derive(Debug)]
pub struct LspDefinitionLocation {
    origin: Option<LspRange>,
    target: LspLocation,
}

impl From<LspLocation> for LspDefinitionLocation {
    fn from(location: LspLocation) -> Self {
        Self {
            origin: None,
            target: location,
        }
    }
}

impl From<LocationLink> for LspDefinitionLocation {
    fn from(location_link: LocationLink) -> Self {
        Self {
            origin: location_link.origin_selection_range,
            target: LspLocation {
                uri: location_link.target_uri,
                // Use target_selection_range (the exact identifier location) instead of
                // target_range (the full declaration range including comments/attributes)
                // to jump directly to the definition name rather than the start of the
                // declaration.
                range: location_link.target_selection_range,
            },
        }
    }
}

#[derive(Debug)]
pub struct DefinitionLocation {
    pub origin: Option<Range>,
    pub target: FileLocation,
}

impl TryFrom<LspDefinitionLocation> for DefinitionLocation {
    type Error = anyhow::Error;

    fn try_from(location: LspDefinitionLocation) -> anyhow::Result<Self> {
        let path = lsp_uri_to_path(&location.target.uri)?;

        Ok(Self {
            origin: location.origin.map(Into::into),
            target: FileLocation {
                path,
                location: location.target.into(),
            },
        })
    }
}

/// A reference location returned from the LSP textDocument/references request.
#[derive(Debug, Clone)]
pub struct ReferenceLocation {
    pub file_path: PathBuf,
    pub range: Range,
}

impl TryFrom<LspLocation> for ReferenceLocation {
    type Error = anyhow::Error;

    fn try_from(location: LspLocation) -> anyhow::Result<Self> {
        let path = lsp_uri_to_path(&location.uri)?;

        Ok(Self {
            file_path: path,
            range: location.range.into(),
        })
    }
}

/// Edit returned by the LSP.
#[derive(Debug, Clone)]
pub struct TextEdit {
    pub range: Range,
    pub text: String,
}

impl From<lsp_types::TextEdit> for TextEdit {
    fn from(edit: lsp_types::TextEdit) -> Self {
        Self {
            range: edit.range.into(),
            text: edit.new_text,
        }
    }
}

/// Document version that should be tracked by the LSP.
pub struct DocumentVersion(i32);

impl DocumentVersion {
    pub fn as_i32(&self) -> i32 {
        self.0
    }
}

impl From<usize> for DocumentVersion {
    fn from(version: usize) -> Self {
        Self(version as i32)
    }
}

#[derive(Debug)]
pub struct TextDocumentContentChangeEvent {
    pub range: Option<Range>,
    pub text: String,
}

impl TextDocumentContentChangeEvent {
    pub fn into_lsp(self) -> lsp_types::TextDocumentContentChangeEvent {
        lsp_types::TextDocumentContentChangeEvent {
            range: self.range.map(|range| range.into_lsp()),
            range_length: None,
            text: self.text,
        }
    }
}

/// Result from a hover request.
#[derive(Debug, Clone)]
pub struct HoverResult {
    /// The hover contents (documentation, type info, etc.)
    pub contents: HoverContents,
    /// The range of the symbol being hovered over, if provided by the server.
    pub range: Option<Range>,
}

/// The kind of markup content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkupKind {
    PlainText,
    Markdown,
}

#[derive(Debug, Clone)]
pub struct HoverContentSection {
    pub value: String,
    pub kind: MarkupKind,
}

/// The contents of a hover response.
#[derive(Debug, Clone)]
pub struct HoverContents {
    pub sections: Vec<HoverContentSection>,
}

impl HoverContents {
    pub fn is_empty(&self) -> bool {
        self.sections.is_empty()
    }
}

impl From<lsp_types::Hover> for HoverResult {
    fn from(hover: lsp_types::Hover) -> Self {
        let contents = match hover.contents {
            lsp_types::HoverContents::Scalar(value) => vec![HoverContentSection {
                value: marked_string_to_string(value),
                kind: MarkupKind::Markdown,
            }],
            lsp_types::HoverContents::Array(values) => values
                .into_iter()
                .map(|value| HoverContentSection {
                    value: marked_string_to_string(value),
                    kind: MarkupKind::Markdown,
                })
                .collect(),
            lsp_types::HoverContents::Markup(content) => {
                let kind = match content.kind {
                    lsp_types::MarkupKind::PlainText => MarkupKind::PlainText,
                    lsp_types::MarkupKind::Markdown => MarkupKind::Markdown,
                };
                vec![HoverContentSection {
                    value: content.value,
                    kind,
                }]
            }
        };

        Self {
            contents: HoverContents { sections: contents },
            range: hover.range.map(Into::into),
        }
    }
}

fn marked_string_to_string(marked: lsp_types::MarkedString) -> String {
    match marked {
        lsp_types::MarkedString::String(s) => s,
        lsp_types::MarkedString::LanguageString(ls) => {
            format!("```{}\n{}\n```", ls.language, ls.value)
        }
    }
}

/// A file change event that can be forwarded to the language server using
/// `workspace/didChangeWatchedFiles`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchedFileChangeEvent {
    pub path: PathBuf,
    pub typ: FileChangeType,
}

impl WatchedFileChangeEvent {
    pub fn into_lsp(self) -> anyhow::Result<FileEvent> {
        Ok(FileEvent {
            uri: path_to_lsp_uri(&self.path)?,
            typ: self.typ,
        })
    }
}

/// The kind of a completion item, mapped from LSP's CompletionItemKind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionKind {
    Text,
    Method,
    Function,
    Constructor,
    Field,
    Variable,
    Class,
    Interface,
    Module,
    Property,
    Unit,
    Value,
    Enum,
    Keyword,
    Snippet,
    Color,
    File,
    Reference,
    Folder,
    EnumMember,
    Constant,
    Struct,
    Event,
    Operator,
    TypeParameter,
}

impl From<CompletionItemKind> for CompletionKind {
    fn from(kind: CompletionItemKind) -> Self {
        match kind {
            CompletionItemKind::TEXT => Self::Text,
            CompletionItemKind::METHOD => Self::Method,
            CompletionItemKind::FUNCTION => Self::Function,
            CompletionItemKind::CONSTRUCTOR => Self::Constructor,
            CompletionItemKind::FIELD => Self::Field,
            CompletionItemKind::VARIABLE => Self::Variable,
            CompletionItemKind::CLASS => Self::Class,
            CompletionItemKind::INTERFACE => Self::Interface,
            CompletionItemKind::MODULE => Self::Module,
            CompletionItemKind::PROPERTY => Self::Property,
            CompletionItemKind::UNIT => Self::Unit,
            CompletionItemKind::VALUE => Self::Value,
            CompletionItemKind::ENUM => Self::Enum,
            CompletionItemKind::KEYWORD => Self::Keyword,
            CompletionItemKind::SNIPPET => Self::Snippet,
            CompletionItemKind::COLOR => Self::Color,
            CompletionItemKind::FILE => Self::File,
            CompletionItemKind::REFERENCE => Self::Reference,
            CompletionItemKind::FOLDER => Self::Folder,
            CompletionItemKind::ENUM_MEMBER => Self::EnumMember,
            CompletionItemKind::CONSTANT => Self::Constant,
            CompletionItemKind::STRUCT => Self::Struct,
            CompletionItemKind::EVENT => Self::Event,
            CompletionItemKind::OPERATOR => Self::Operator,
            CompletionItemKind::TYPE_PARAMETER => Self::TypeParameter,
            _ => Self::Text,
        }
    }
}

/// A single completion item returned from an LSP completion request.
#[derive(Debug, Clone)]
pub struct CompletionItemData {
    /// The label displayed in the completion menu.
    pub label: String,
    /// Additional detail (e.g. type signature), displayed dimmed.
    pub detail: Option<String>,
    /// The kind of completion (function, variable, etc.).
    pub kind: Option<CompletionKind>,
    /// Text used for filtering. Falls back to `label` if None.
    pub filter_text: Option<String>,
    /// Text to sort completions by. Falls back to `label` if None.
    pub sort_text: Option<String>,
    /// The text to insert when this completion is accepted.
    pub insert_text: String,
    /// Whether the insert text is a snippet (contains tab stops like $1, ${2:placeholder}).
    pub is_snippet: bool,
    /// The range of text to replace when applying this completion.
    /// If None, the editor should replace the current word prefix.
    pub text_edit_range: Option<Range>,
    /// Additional text edits (e.g. auto-imports) applied alongside the main insertion.
    pub additional_edits: Vec<TextEdit>,
    /// The original LSP CompletionItem, preserved for resolve requests.
    pub raw_item: CompletionItem,
}

impl CompletionItemData {
    pub fn effective_filter_text(&self) -> &str {
        self.filter_text.as_deref().unwrap_or(&self.label)
    }

    pub fn effective_sort_text(&self) -> &str {
        self.sort_text.as_deref().unwrap_or(&self.label)
    }
}

/// The result of a completion request.
#[derive(Debug, Clone)]
pub struct CompletionResult {
    /// The completion items.
    pub items: Vec<CompletionItemData>,
    /// Whether the list is incomplete (server may have more results if the user continues typing).
    pub is_incomplete: bool,
}

impl From<CompletionResponse> for CompletionResult {
    fn from(response: CompletionResponse) -> Self {
        match response {
            CompletionResponse::Array(items) => Self {
                items: items.into_iter().map(completion_item_to_data).collect(),
                is_incomplete: false,
            },
            CompletionResponse::List(list) => Self {
                items: list
                    .items
                    .into_iter()
                    .map(completion_item_to_data)
                    .collect(),
                is_incomplete: list.is_incomplete,
            },
        }
    }
}

fn completion_item_to_data(item: CompletionItem) -> CompletionItemData {
    let is_snippet = item.insert_text_format == Some(InsertTextFormat::SNIPPET);

    let (insert_text, text_edit_range) = if let Some(ref text_edit) = item.text_edit {
        match text_edit {
            lsp_types::CompletionTextEdit::Edit(edit) => {
                (edit.new_text.clone(), Some(edit.range.into()))
            }
            lsp_types::CompletionTextEdit::InsertAndReplace(edit) => {
                (edit.new_text.clone(), Some(edit.insert.into()))
            }
        }
    } else {
        let text = item
            .insert_text
            .clone()
            .unwrap_or_else(|| item.label.clone());
        (text, None)
    };

    let additional_edits = item
        .additional_text_edits
        .as_ref()
        .map(|edits| edits.iter().map(|e| TextEdit::from(e.clone())).collect())
        .unwrap_or_default();

    CompletionItemData {
        label: item.label.clone(),
        detail: item.detail.clone(),
        kind: item.kind.map(Into::into),
        filter_text: item.filter_text.clone(),
        sort_text: item.sort_text.clone(),
        insert_text,
        is_snippet,
        text_edit_range,
        additional_edits,
        raw_item: item,
    }
}

/// Trigger context for a completion request.
#[derive(Debug, Clone)]
pub enum CompletionTrigger {
    /// User explicitly invoked completion (e.g. Ctrl+Space).
    Invoked,
    /// A trigger character was typed (e.g. '.', '::', '->').
    TriggerCharacter(String),
    /// Re-triggered for an incomplete completion list as the user continues typing.
    Incomplete,
}

/// The result of a signature help request.
#[derive(Debug, Clone)]
pub struct SignatureHelpResult {
    pub signatures: Vec<SignatureInfo>,
    pub active_signature: Option<usize>,
    pub active_parameter: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct SignatureInfo {
    pub label: String,
    pub documentation: Option<String>,
    pub parameters: Vec<ParameterInfo>,
}

#[derive(Debug, Clone)]
pub struct ParameterInfo {
    pub label: ParameterLabel,
    pub documentation: Option<String>,
}

#[derive(Debug, Clone)]
pub enum ParameterLabel {
    Simple(String),
    Offsets(u32, u32),
}

impl From<lsp_types::SignatureHelp> for SignatureHelpResult {
    fn from(help: lsp_types::SignatureHelp) -> Self {
        Self {
            signatures: help.signatures.into_iter().map(Into::into).collect(),
            active_signature: help.active_signature.map(|s| s as usize),
            active_parameter: help.active_parameter.map(|p| p as usize),
        }
    }
}

impl From<lsp_types::SignatureInformation> for SignatureInfo {
    fn from(sig: lsp_types::SignatureInformation) -> Self {
        Self {
            label: sig.label,
            documentation: sig.documentation.map(|doc| match doc {
                lsp_types::Documentation::String(s) => s,
                lsp_types::Documentation::MarkupContent(m) => m.value,
            }),
            parameters: sig
                .parameters
                .unwrap_or_default()
                .into_iter()
                .map(Into::into)
                .collect(),
        }
    }
}

impl From<lsp_types::ParameterInformation> for ParameterInfo {
    fn from(param: lsp_types::ParameterInformation) -> Self {
        Self {
            label: match param.label {
                lsp_types::ParameterLabel::Simple(s) => ParameterLabel::Simple(s),
                lsp_types::ParameterLabel::LabelOffsets(offsets) => {
                    ParameterLabel::Offsets(offsets[0], offsets[1])
                }
            },
            documentation: param.documentation.map(|doc| match doc {
                lsp_types::Documentation::String(s) => s,
                lsp_types::Documentation::MarkupContent(m) => m.value,
            }),
        }
    }
}

/// A code action returned from the LSP.
#[derive(Debug, Clone)]
pub struct CodeActionData {
    pub title: String,
    pub kind: Option<String>,
    pub is_preferred: bool,
    pub raw: lsp_types::CodeActionOrCommand,
}

impl From<lsp_types::CodeActionOrCommand> for CodeActionData {
    fn from(action_or_cmd: lsp_types::CodeActionOrCommand) -> Self {
        match &action_or_cmd {
            lsp_types::CodeActionOrCommand::CodeAction(action) => Self {
                title: action.title.clone(),
                kind: action.kind.as_ref().map(|k| k.as_str().to_string()),
                is_preferred: action.is_preferred.unwrap_or(false),
                raw: action_or_cmd,
            },
            lsp_types::CodeActionOrCommand::Command(cmd) => Self {
                title: cmd.title.clone(),
                kind: None,
                is_preferred: false,
                raw: action_or_cmd,
            },
        }
    }
}

/// The result of a prepare-rename request.
#[derive(Debug, Clone)]
pub struct PrepareRenameResult {
    pub range: Range,
    pub placeholder: Option<String>,
}

/// The result of a rename request — a set of edits across files.
#[derive(Debug, Clone)]
pub struct RenameResult {
    pub edits: Vec<FileEdits>,
}

/// Edits for a single file as part of a workspace edit.
#[derive(Debug, Clone)]
pub struct FileEdits {
    pub path: PathBuf,
    pub edits: Vec<TextEdit>,
}
