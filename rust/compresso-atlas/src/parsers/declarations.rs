//! Shared pieces of the hand-written Java and C# declaration walks (E17-40,
//! D105): the owner of a member, the bounds on nesting and qualified-name
//! length, and the collection of symbols in source order with the byte
//! ranges call attribution needs.

use std::rc::Rc;

use compresso_store::{SymbolKind, SymbolRecord};
use tree_sitter::Node;

use super::{Extraction, line_number, node_text};

/// Types nested deeper than this are not symbols, and neither are their
/// members: a qualified name repeats every outer type, so names would grow
/// with depth.
pub(crate) const MAX_TYPE_DEPTH: usize = 32;
/// A qualified name longer than this is not stored, and a type whose name
/// is skipped takes its members with it.
pub(crate) const MAX_QUALIFIED_NAME_BYTES: usize = 1_024;

/// The type a member belongs to: its qualified name (`Outer::Inner`) and
/// how deeply it is nested.
#[derive(Clone, Debug)]
pub(super) struct Owner {
    pub(super) qualified: Rc<str>,
    depth: usize,
}

/// Symbols found by a walk, with their nodes' byte ranges, in any order.
#[derive(Default)]
pub(super) struct Found {
    entries: Vec<(usize, usize, SymbolRecord)>,
}

impl Found {
    /// Records a type and returns it as the owner of its members, unless it
    /// is nested too deeply or its qualified name is too long.
    pub(super) fn type_symbol(
        &mut self,
        node: Node<'_>,
        kind: SymbolKind,
        name: String,
        owner: Option<&Owner>,
        signature: Option<String>,
    ) -> Option<Owner> {
        let depth = owner.map_or(1, |owner| owner.depth + 1);
        if depth > MAX_TYPE_DEPTH {
            return None;
        }
        let qualified = qualify(owner, &name)?;
        let inner = Owner {
            qualified: Rc::from(qualified.as_str()),
            depth,
        };
        self.push(node, name, qualified, kind, signature);
        Some(inner)
    }

    /// Records a member of `owner` (`Owner::name`).
    pub(super) fn member(
        &mut self,
        node: Node<'_>,
        kind: SymbolKind,
        name: &str,
        owner: &Owner,
        signature: Option<String>,
    ) {
        if let Some(qualified) = qualify(Some(owner), name) {
            self.push(node, name.to_owned(), qualified, kind, signature);
        }
    }

    /// Records a package or namespace declaration under its dotted name.
    pub(super) fn module(&mut self, node: Node<'_>, name: String, signature: String) {
        if name.len() <= MAX_QUALIFIED_NAME_BYTES {
            let signature = super::bounded_signature(&signature);
            self.push(node, name.clone(), name, SymbolKind::Module, signature);
        }
    }

    fn push(
        &mut self,
        node: Node<'_>,
        name: String,
        qualified_name: String,
        kind: SymbolKind,
        signature: Option<String>,
    ) {
        self.entries.push((
            node.start_byte(),
            node.end_byte(),
            SymbolRecord {
                name,
                qualified_name,
                kind,
                signature,
                start_line: line_number(node.start_position().row),
                end_line: line_number(node.end_position().row),
                token_estimate: 0,
            },
        ));
    }

    /// Hands the symbols over in source order (an outer declaration before
    /// the ones it contains), each with its node's byte range.
    pub(super) fn finish(
        mut self,
        output: &mut Extraction,
        symbol_bytes: &mut Vec<Option<(usize, usize)>>,
    ) {
        self.entries
            .sort_by(|left, right| left.0.cmp(&right.0).then_with(|| right.1.cmp(&left.1)));
        for (start, end, symbol) in self.entries {
            output.symbols.push(symbol);
            symbol_bytes.push(Some((start, end)));
        }
    }
}

/// The declaration text from `node`'s start to `end` (review S4). When it
/// exceeds the signature bound and `prefix` (the declaration's modifier and
/// annotation or attribute nodes) holds annotations, they are left out and
/// the modifier keywords kept, then the rest of the text, so a long
/// annotation never pushes `public` or `static` past the bound that
/// visibility is read from. Each part is cut to the bound before it is
/// copied.
pub(super) fn declaration_text(
    node: Node<'_>,
    end: usize,
    prefix: &[Node<'_>],
    is_annotation: fn(&Node<'_>) -> bool,
    source: &str,
) -> Option<String> {
    let text = source.get(node.start_byte()..end)?;
    // A kept part that was cut ends with the mark, so the signature built
    // from it is marked even when it trims below the bound.
    let kept = |text: &str| {
        let mut kept = super::signature_prefix(text).to_owned();
        if super::is_cut(text) {
            kept.push('…');
        }
        kept
    };
    if text.trim().len() <= super::MAX_SIGNATURE_BYTES || !prefix.iter().any(is_annotation) {
        return Some(kept(text.trim()));
    }
    let mut keywords = String::new();
    for word in prefix
        .iter()
        .filter(|child| !is_annotation(child))
        .filter_map(|child| node_text(*child, source))
    {
        if keywords.len() + word.len() > super::MAX_SIGNATURE_BYTES {
            break;
        }
        if !keywords.is_empty() {
            keywords.push(' ');
        }
        keywords.push_str(word.trim());
    }
    let after = prefix
        .iter()
        .map(|child| child.end_byte())
        .max()
        .unwrap_or(node.start_byte())
        .min(end);
    let rest = kept(source.get(after..end)?.trim());
    Some(if keywords.is_empty() {
        rest
    } else {
        format!("{keywords} {rest}")
    })
}

/// The `module` symbols a walk records, without repeats, in source order.
pub(super) fn module_names(
    walk: super::languages::Walk,
    root: Node<'_>,
    source: &str,
) -> Vec<String> {
    let mut extraction = Extraction {
        symbols: Vec::new(),
        imports: Vec::new(),
        calls: Vec::new(),
        calls_truncated: false,
        symbols_truncated: false,
    };
    let mut bytes = Vec::new();
    walk(root, source, &mut extraction, &mut bytes);
    let mut seen = std::collections::HashSet::new();
    extraction
        .symbols
        .into_iter()
        .filter(|symbol| symbol.kind == SymbolKind::Module)
        .map(|symbol| symbol.qualified_name)
        .filter(|name| seen.insert(name.clone()))
        .collect()
}

fn qualify(owner: Option<&Owner>, name: &str) -> Option<String> {
    let qualified = match owner {
        Some(owner) => format!("{}::{name}", owner.qualified),
        None => name.to_owned(),
    };
    (qualified.len() <= MAX_QUALIFIED_NAME_BYTES).then_some(qualified)
}

/// A name made of identifier characters (`_`, `$`, letters, digits).
pub(super) fn is_identifier(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_QUALIFIED_NAME_BYTES
        && name
            .chars()
            .all(|character| character == '_' || character == '$' || character.is_alphanumeric())
}

/// A dotted name (`com.example.store`, `System.Text`) as written, without
/// whitespace or comments between its parts; `None` unless every part is an
/// identifier. Longer text than a stored name is rejected before it is
/// copied.
pub(super) fn dotted_text(node: Node<'_>, source: &str) -> Option<String> {
    if node.byte_range().len() > MAX_QUALIFIED_NAME_BYTES * 4 {
        return None;
    }
    let text = node_text(node, source)?;
    let mut dotted = String::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        let trimmed = rest.trim_start();
        if let Some(comment) = trimmed.strip_prefix("/*") {
            rest = comment.split_once("*/").map_or("", |(_, after)| after);
        } else if trimmed.starts_with("//") {
            rest = trimmed.split_once('\n').map_or("", |(_, after)| after);
        } else {
            let mut characters = trimmed.chars();
            let Some(character) = characters.next() else {
                break;
            };
            dotted.push(character);
            rest = characters.as_str();
        }
    }
    let valid = dotted.len() <= MAX_QUALIFIED_NAME_BYTES && dotted.split('.').all(is_identifier);
    valid.then_some(dotted)
}

/// Java and C# modifier words.
const MODIFIER_WORDS: [&str; 25] = [
    "abstract",
    "async",
    "const",
    "default",
    "extern",
    "file",
    "final",
    "internal",
    "native",
    "new",
    "transient",
    "override",
    "partial",
    "private",
    "protected",
    "public",
    "readonly",
    "required",
    "sealed",
    "static",
    "strictfp",
    "synchronized",
    "unsafe",
    "virtual",
    "volatile",
];

/// The modifier words at the start of a Java or C# signature, after its
/// annotations (`@Override`, `@Named("x")`) and attributes (`[Obsolete]`).
pub(crate) fn modifiers(signature: Option<&str>) -> Vec<&str> {
    let mut rest = signature.unwrap_or_default();
    let mut words = Vec::new();
    loop {
        rest = rest.trim_start();
        if let Some(after) = rest.strip_prefix('@') {
            let end = after
                .find(|character: char| {
                    !(character.is_alphanumeric() || matches!(character, '_' | '$' | '.'))
                })
                .unwrap_or(after.len());
            rest = &after[end..];
            if rest.trim_start().starts_with('(') {
                rest = skip_group(rest.trim_start(), '(', ')');
            }
            continue;
        }
        if rest.starts_with('[') {
            rest = skip_group(rest, '[', ']');
            continue;
        }
        let end = rest
            .find(|character: char| !(character.is_alphanumeric() || character == '_'))
            .unwrap_or(rest.len());
        let word = &rest[..end];
        if word.is_empty() || !MODIFIER_WORDS.contains(&word) {
            return words;
        }
        words.push(word);
        rest = &rest[end..];
    }
}

/// The text after one balanced group starting at `text`'s first byte,
/// skipping quoted strings inside it; the whole text when it never closes.
fn skip_group(text: &str, open: char, close: char) -> &str {
    let mut depth = 0_usize;
    let mut quote = None;
    let mut escaped = false;
    for (index, character) in text.char_indices() {
        if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == delimiter {
                quote = None;
            }
            continue;
        }
        match character {
            '"' | '\'' => quote = Some(character),
            _ if character == open => depth += 1,
            _ if character == close => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return &text[index + character.len_utf8()..];
                }
            }
            _ => {}
        }
    }
    ""
}

#[cfg(test)]
mod tests {
    use super::modifiers;

    #[test]
    fn test_modifiers_skip_annotations_and_attributes() {
        assert_eq!(
            modifiers(Some("@Override\n    public static <R> R load()")),
            ["public", "static"]
        );
        assert_eq!(
            modifiers(Some("@Named(value = \"private (x)\") protected void run()")),
            ["protected"]
        );
        assert_eq!(
            modifiers(Some(
                "[Obsolete(\"]\")][Pure] internal sealed partial class Store"
            )),
            ["internal", "sealed", "partial"]
        );
        assert_eq!(modifiers(Some("void run()")), Vec::<&str>::new());
        assert_eq!(modifiers(None), Vec::<&str>::new());
        // An unclosed annotation never panics or loops.
        assert_eq!(modifiers(Some("@Bad(\"open public")), Vec::<&str>::new());
    }
}
