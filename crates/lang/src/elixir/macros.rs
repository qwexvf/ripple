//! Generic Elixir macro-shape scanning — no framework knowledge.
//!
//! Elixir has no dedicated declaration nodes. `defmodule`, `def`, and every DSL
//! built on it (Absinthe, Ecto, Ash, Phoenix router) are macro `call`s of the
//! same shape: `name :atom, opts do ... end`. So the *shape* can be read once,
//! generically, and each framework only needs a table saying which shapes carry
//! meaning — see [`super::dsl`]. Adding a DSL adds a table, not a walker.

use std::collections::HashMap;
use tree_sitter::Node as TsNode;

/// A referenced module function: (module FQN, function name).
pub type FunRef = (String, String);

/// One macro call, plus the chain of macro blocks enclosing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroCall {
    /// The macro name (`field`, `object`, `resolve`, `import_fields`, …).
    pub name: String,
    /// Enclosing macro blocks, outermost first. Each entry is `macro` or
    /// `macro:atom` — `["object:player_queries", "field:current_player"]`.
    pub scope: Vec<String>,
    /// Leading atom arguments, `:` stripped.
    pub atoms: Vec<String>,
    /// String arguments, quotes included.
    pub strings: Vec<String>,
    /// Module (alias) arguments, FQN-resolved.
    pub modules: Vec<String>,
    /// Module functions named in the call's own arguments (`&M.f/3`).
    pub fun_refs: Vec<FunRef>,
    /// Keyword options naming a module function (`resolve: &M.f/3`).
    pub keyword_fun_refs: Vec<(String, FunRef)>,
    pub line: u32,
}

impl MacroCall {
    /// The scope entry this call opens, whether or not it has a block.
    fn scope_entry(&self) -> String {
        match self.atoms.first() {
            Some(atom) => format!("{}:{atom}", self.name),
            None => self.name.clone(),
        }
    }

    /// Scope chain a child block of this call would see.
    pub fn inner_scope(&self) -> Vec<String> {
        let mut inner = self.scope.clone();
        inner.push(self.scope_entry());
        inner
    }
}

/// A call to another module's function.
#[derive(Debug, Clone)]
pub struct RemoteCall {
    /// Target module FQN, alias-resolved.
    pub module: String,
    pub func: String,
    /// Module arguments passed to it (`Repo.get(Player, id)` → `["Player"]`).
    pub modules: Vec<String>,
    pub line: u32,
}

/// Everything one file's AST says about macros, in document order.
#[derive(Debug, Default)]
pub struct Scan {
    /// local alias name → module FQN (`alias A.B.C` → `C` → `A.B.C`)
    pub aliases: HashMap<String, String>,
    pub calls: Vec<MacroCall>,
    pub remote_calls: Vec<RemoteCall>,
    /// `%Player{}` struct references: (module FQN, line).
    pub struct_refs: Vec<(String, u32)>,
}

/// Scan a file's AST. Module names are FQN-resolved through the file's aliases,
/// so callers never deal with Elixir alias semantics.
pub fn scan(root: TsNode, src: &[u8]) -> Scan {
    let mut scan = Scan::default();
    let mut scope = Vec::new();
    walk(root, src, &mut scope, &mut scan);
    resolve_aliases(&mut scan);
    scan
}

fn text<'a>(n: TsNode, src: &'a [u8]) -> &'a str {
    n.utf8_text(src).unwrap_or("")
}

fn line_of(n: TsNode) -> u32 {
    n.start_position().row as u32 + 1
}

fn walk(node: TsNode, src: &[u8], scope: &mut Vec<String>, out: &mut Scan) {
    let mut opened = false;
    match node.kind() {
        "call" => match node.child_by_field_name("target") {
            Some(t) if t.kind() == "identifier" => {
                let call = macro_call(node, text(t, src), scope, src);
                if text(t, src) == "alias" {
                    collect_alias(node, src, out);
                }
                if has_block(node) {
                    scope.push(call.scope_entry());
                    opened = true;
                }
                out.calls.push(call);
            }
            Some(t) if t.kind() == "dot" => {
                if let Some(rc) = remote_call(node, t, src) {
                    out.remote_calls.push(rc);
                }
            }
            _ => {}
        },
        "struct" => {
            if let Some(a) = node.named_child(0).filter(|a| a.kind() == "alias") {
                out.struct_refs
                    .push((text(a, src).to_owned(), line_of(node)));
            }
        }
        _ => {}
    }

    let mut c = node.walk();
    for child in node.children(&mut c) {
        walk(child, src, scope, out);
    }
    if opened {
        scope.pop();
    }
}

fn macro_call(node: TsNode, name: &str, scope: &[String], src: &[u8]) -> MacroCall {
    let mut call = MacroCall {
        name: name.to_owned(),
        scope: scope.to_vec(),
        atoms: Vec::new(),
        strings: Vec::new(),
        modules: Vec::new(),
        fun_refs: Vec::new(),
        keyword_fun_refs: Vec::new(),
        line: line_of(node),
    };
    let Some(args) = args_node(node) else {
        return call;
    };

    let mut c = args.walk();
    for arg in args.named_children(&mut c) {
        match arg.kind() {
            "atom" => call
                .atoms
                .push(text(arg, src).trim_start_matches(':').to_owned()),
            "string" => call.strings.push(text(arg, src).to_owned()),
            "keywords" => collect_keywords(arg, src, &mut call),
            _ => {}
        }
    }
    // aliases and function captures can sit anywhere in the argument list
    // (`from p in Team`, `resolve(&M.f/3)`), so collect them by descent
    collect_arg_refs(args, src, &mut call);
    call
}

fn collect_keywords(keywords: TsNode, src: &[u8], call: &mut MacroCall) {
    let mut c = keywords.walk();
    for pair in keywords.named_children(&mut c) {
        let (Some(k), Some(v)) = (
            pair.child_by_field_name("key"),
            pair.child_by_field_name("value"),
        ) else {
            continue;
        };
        // the key node's text carries its colon and trailing space: `resolve: `
        let key = text(k, src).trim().trim_end_matches(':').to_owned();
        if let Some(r) = fun_ref(v, src) {
            call.keyword_fun_refs.push((key, r));
        }
    }
}

/// Module names and `&M.f/N` captures inside an argument list. Skips inline
/// `fn ... end` bodies: what they call is a plain remote call, not a reference
/// the enclosing macro names.
fn collect_arg_refs(node: TsNode, src: &[u8], call: &mut MacroCall) {
    if node.kind() == "anonymous_function" {
        return;
    }
    match node.kind() {
        "alias" => call.modules.push(text(node, src).to_owned()),
        "dot" => {
            if let (Some(l), Some(r)) = (
                node.child_by_field_name("left"),
                node.child_by_field_name("right"),
            ) {
                if l.kind() == "alias" && r.kind() == "identifier" {
                    call.fun_refs
                        .push((text(l, src).to_owned(), text(r, src).to_owned()));
                }
            }
        }
        _ => {}
    }
    let mut c = node.walk();
    for child in node.children(&mut c) {
        collect_arg_refs(child, src, call);
    }
}

/// The single module function a keyword value names, if it names one.
fn fun_ref(node: TsNode, src: &[u8]) -> Option<FunRef> {
    let mut probe = MacroCall {
        name: String::new(),
        scope: Vec::new(),
        atoms: Vec::new(),
        strings: Vec::new(),
        modules: Vec::new(),
        fun_refs: Vec::new(),
        keyword_fun_refs: Vec::new(),
        line: 0,
    };
    collect_arg_refs(node, src, &mut probe);
    probe.fun_refs.into_iter().next()
}

fn remote_call(node: TsNode, dot: TsNode, src: &[u8]) -> Option<RemoteCall> {
    let (l, r) = (
        dot.child_by_field_name("left")?,
        dot.child_by_field_name("right")?,
    );
    if l.kind() != "alias" || r.kind() != "identifier" {
        return None;
    }
    let mut modules = Vec::new();
    if let Some(args) = args_node(node) {
        let mut c = args.walk();
        for arg in args.named_children(&mut c) {
            if arg.kind() == "alias" {
                modules.push(text(arg, src).to_owned());
            }
        }
    }
    Some(RemoteCall {
        module: text(l, src).to_owned(),
        func: text(r, src).to_owned(),
        modules,
        line: line_of(node),
    })
}

fn args_node(call: TsNode) -> Option<TsNode> {
    let mut c = call.walk();
    let found = call.children(&mut c).find(|n| n.kind() == "arguments");
    found
}

fn has_block(call: TsNode) -> bool {
    let mut c = call.walk();
    // bound to a local so the cursor's borrow ends before returning
    let found = call.children(&mut c).any(|n| n.kind() == "do_block");
    found
}

fn collect_alias(call: TsNode, src: &[u8], out: &mut Scan) {
    let Some(arg) = args_node(call).and_then(|a| a.named_child(0)) else {
        return;
    };
    match arg.kind() {
        "alias" => {
            let fqn = text(arg, src);
            if let Some(last) = fqn.rsplit('.').next() {
                out.aliases.insert(last.to_owned(), fqn.to_owned());
            }
        }
        // `alias A.{B, C}`
        "dot" => {
            let (Some(l), Some(r)) = (
                arg.child_by_field_name("left"),
                arg.child_by_field_name("right"),
            ) else {
                return;
            };
            if r.kind() != "tuple" {
                return;
            }
            let prefix = text(l, src);
            let mut c = r.walk();
            for t in r.named_children(&mut c) {
                if t.kind() == "alias" {
                    let name = text(t, src);
                    out.aliases
                        .insert(name.to_owned(), format!("{prefix}.{name}"));
                }
            }
        }
        _ => {}
    }
}

/// Expand local alias names to FQNs once the whole alias table is known, so no
/// caller has to understand Elixir aliases.
fn resolve_aliases(scan: &mut Scan) {
    let aliases = std::mem::take(&mut scan.aliases);
    for call in &mut scan.calls {
        for m in &mut call.modules {
            *m = resolve_module(m, &aliases);
        }
        for (m, _) in &mut call.fun_refs {
            *m = resolve_module(m, &aliases);
        }
        for (_, (m, _)) in &mut call.keyword_fun_refs {
            *m = resolve_module(m, &aliases);
        }
    }
    for rc in &mut scan.remote_calls {
        rc.module = resolve_module(&rc.module, &aliases);
        for m in &mut rc.modules {
            *m = resolve_module(m, &aliases);
        }
    }
    for (m, _) in &mut scan.struct_refs {
        *m = resolve_module(m, &aliases);
    }
    scan.aliases = aliases;
}

/// Resolve an Elixir module expression (possibly an alias local name) to a FQN.
pub fn resolve_module(expr: &str, aliases: &HashMap<String, String>) -> String {
    if let Some(fqn) = aliases.get(expr) {
        return fqn.clone();
    }
    if let Some((head, rest)) = expr.split_once('.') {
        if let Some(fqn) = aliases.get(head) {
            return format!("{fqn}.{rest}");
        }
    }
    expr.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LanguageAdapter;

    fn scan_src(src: &str) -> Scan {
        let mut p = tree_sitter::Parser::new();
        p.set_language(&crate::elixir::Adapter::new().grammar())
            .expect("elixir grammar");
        let tree = p.parse(src, None).expect("parse");
        scan(tree.root_node(), src.as_bytes())
    }

    /// A DSL the scanner has never heard of still yields its declarations,
    /// nesting and module references — that's the point of the generic layer.
    #[test]
    fn scans_an_unknown_dsl() {
        let s = scan_src(
            "defmodule My.Resource do\n  alias My.Checks.Owner\n  actions do\n    create :register, handler: &Owner.register/2 do\n      accept [:email]\n    end\n  end\nend\n",
        );
        let create = s
            .calls
            .iter()
            .find(|c| c.name == "create")
            .expect("create macro call");
        assert_eq!(create.scope, ["defmodule", "actions"]);
        assert_eq!(create.atoms, ["register"]);
        // the keyword's `&Owner.register/2` is alias-resolved without any
        // framework-specific code
        assert_eq!(
            create.keyword_fun_refs,
            vec![(
                "handler".to_owned(),
                ("My.Checks.Owner".to_owned(), "register".to_owned())
            )]
        );
        // a nested macro sees the enclosing chain
        let accept = s.calls.iter().find(|c| c.name == "accept").expect("accept");
        assert_eq!(accept.scope, ["defmodule", "actions", "create:register"]);
    }

    #[test]
    fn resolve_module_expands_local_alias_names() {
        let mut al = HashMap::new();
        al.insert(
            "PlayerResolver".to_string(),
            "App.Resolvers.PlayerResolver".to_string(),
        );
        assert_eq!(
            resolve_module("PlayerResolver", &al),
            "App.Resolvers.PlayerResolver"
        );
        assert_eq!(
            resolve_module("PlayerResolver.Nested", &al),
            "App.Resolvers.PlayerResolver.Nested"
        );
        assert_eq!(resolve_module("Unknown", &al), "Unknown");
    }

    #[test]
    fn resolves_aliases_in_remote_calls_and_structs() {
        let s = scan_src(
            "defmodule S do\n  alias App.{Players, Player}\n  def show(id) do\n    Players.get(Player, id)\n    %Player{}\n  end\nend\n",
        );
        let rc = s
            .remote_calls
            .iter()
            .find(|r| r.func == "get")
            .expect("remote call");
        assert_eq!(rc.module, "App.Players");
        assert_eq!(rc.modules, vec!["App.Player"]);
        assert_eq!(
            s.struct_refs
                .iter()
                .map(|(m, _)| m.as_str())
                .collect::<Vec<_>>(),
            vec!["App.Player"]
        );
    }

    /// An inline `fn` names no single function, so nothing is attributed to the
    /// enclosing macro; the body's call is still a remote call.
    #[test]
    fn inline_fn_is_not_a_named_reference() {
        let s = scan_src(
            "defmodule S do\n  field :rank, resolve: fn p, _, _ -> Stats.rank(p) end\nend\n",
        );
        let field = s.calls.iter().find(|c| c.name == "field").expect("field");
        assert!(field.keyword_fun_refs.is_empty());
        assert!(field.fun_refs.is_empty());
        assert!(s
            .remote_calls
            .iter()
            .any(|r| r.module == "Stats" && r.func == "rank"));
    }
}
