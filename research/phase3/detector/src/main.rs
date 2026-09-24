use proc_macro2::{LineColumn, Span};
use quote::ToTokens;
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{
    Expr, ExprAssign, ExprAsync, ExprAwait, ExprBinary, ExprCall, ExprClosure, ExprMacro,
    ExprMethodCall, ExprUnary, ItemFn, Local, Pat, Stmt,
};

#[derive(Clone, Debug)]
struct Mutation {
    at: LineColumn,
    target: String,
    kind: String,
    status_like: bool,
}
#[derive(Clone, Debug)]
struct AwaitPoint {
    at: LineColumn,
    text: String,
}
#[derive(Clone, Debug)]
struct Action {
    at: LineColumn,
    target: String,
    kind: String,
    text: String,
}
#[derive(Clone, Debug)]
struct Guard {
    at: LineColumn,
    target_tokens: String,
    cleanup_tokens: String,
}
#[derive(Clone, Debug)]
struct Hit {
    path: String,
    line: usize,
    scope: String,
    mutation: String,
    target: String,
    await_line: usize,
    recovery: String,
}

const MUTATING_METHODS: &[&str] = &[
    "take",
    "pop",
    "pop_front",
    "pop_back",
    "remove",
    "replace",
    "swap",
    "store",
    "fetch_add",
    "fetch_sub",
    "fetch_or",
    "fetch_and",
    "fetch_xor",
    "set",
];
const RESTORE_METHODS: &[&str] = &[
    "insert",
    "push",
    "push_front",
    "push_back",
    "replace",
    "store",
    "set",
    "restore",
    "put",
    "put_back",
    "reinsert",
    "swap",
    "fetch_add",
    "fetch_sub",
];
const TERMINAL_METHODS: &[&str] = &[
    "finish",
    "on_finish",
    "commit",
    "ack",
    "send",
    "flush",
    "complete",
    "finalize",
    "close",
    "write_all",
    "write",
];

fn tokens<T: ToTokens>(value: &T) -> String {
    value.to_token_stream().to_string()
}
fn compact(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}
fn loc(span: Span) -> LineColumn {
    span.start()
}
fn line_of(span: Span) -> usize {
    loc(span).line
}
fn before(a: LineColumn, b: LineColumn) -> bool {
    (a.line, a.column) < (b.line, b.column)
}
fn after(a: LineColumn, b: LineColumn) -> bool {
    before(b, a)
}

fn ident_from_pat(pat: &Pat) -> Option<String> {
    match pat {
        Pat::Ident(p) => Some(p.ident.to_string()),
        Pat::Reference(p) => ident_from_pat(&p.pat),
        Pat::Type(p) => ident_from_pat(&p.pat),
        Pat::Paren(p) => ident_from_pat(&p.pat),
        _ => None,
    }
}

fn state_key(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Path(p) => Some(compact(&tokens(&p.path))),
        Expr::Field(f) => Some(format!(
            "{}.{}",
            state_key(&f.base)?,
            f.member.to_token_stream()
        )),
        Expr::Index(i) => Some(format!("{}[]", state_key(&i.expr)?)),
        Expr::Reference(r) => state_key(&r.expr),
        Expr::Unary(u) if matches!(u.op, syn::UnOp::Deref(_)) => state_key(&u.expr),
        Expr::Paren(p) => state_key(&p.expr),
        Expr::Group(g) => state_key(&g.expr),
        Expr::MethodCall(m)
            if matches!(
                m.method.to_string().as_str(),
                "lock"
                    | "read"
                    | "write"
                    | "borrow"
                    | "borrow_mut"
                    | "as_mut"
                    | "as_ref"
                    | "unwrap"
                    | "expect"
                    | "deref"
                    | "deref_mut"
                    | "get_mut"
                    | "as_deref_mut"
                    | "as_deref"
            ) =>
        {
            state_key(&m.receiver)
        }
        _ => None,
    }
}

fn is_shared_type(ty: &syn::Type) -> bool {
    let s = tokens(ty);
    s.contains('&')
        || [
            "Arc", "Mutex", "RwLock", "Atomic", "DashMap", "RefCell", "Cell", "Guard",
        ]
        .iter()
        .any(|needle| s.contains(needle))
}

#[derive(Default)]
struct ScopeFacts {
    mutations: Vec<Mutation>,
    awaits: Vec<AwaitPoint>,
    actions: Vec<Action>,
    guards: Vec<Guard>,
    locals: HashSet<String>,
    shared: HashSet<String>,
    candidates: Vec<Hit>,
    drop_impls: HashMap<String, String>,
}

struct FactsCollector<'a> {
    facts: &'a mut ScopeFacts,
}
impl<'ast> Visit<'ast> for FactsCollector<'_> {
    fn visit_expr_async(&mut self, _: &'ast ExprAsync) { /* async bodies are separate scopes */
    }
    fn visit_expr_closure(&mut self, node: &'ast ExprClosure) {
        if node.asyncness.is_some() {
            return;
        }
        visit::visit_expr_closure(self, node);
    }
    fn visit_item_fn(&mut self, _: &'ast ItemFn) { /* nested items are separate scopes */
    }

    fn visit_expr_await(&mut self, node: &'ast ExprAwait) {
        self.facts.awaits.push(AwaitPoint {
            at: loc(node.span()),
            text: compact(&tokens(&node.base)),
        });
        visit::visit_expr_await(self, node);
    }
    fn visit_local(&mut self, node: &'ast Local) {
        let local_name = ident_from_pat(&node.pat);
        if let Some(name) = &local_name {
            self.facts.locals.insert(name.clone());
            if let Pat::Type(p) = &node.pat {
                if is_shared_type(&p.ty) {
                    self.facts.shared.insert(name.clone());
                }
            }
        }
        if let Some(init) = &node.init {
            let init_text = compact(&tokens(&init.expr));
            let type_name = match &*init.expr {
                Expr::Struct(s) => s.path.segments.last().map(|s| s.ident.to_string()),
                Expr::Call(c) => match &*c.func {
                    Expr::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()),
                    _ => None,
                },
                _ => None,
            }
            .or_else(|| match &node.pat {
                Pat::Type(p) => match &*p.ty {
                    syn::Type::Path(t) => t.path.segments.last().map(|s| s.ident.to_string()),
                    _ => None,
                },
                _ => None,
            });
            if let Some(drop_body) =
                type_name.and_then(|name| self.facts.drop_impls.get(&name).cloned())
            {
                let combined = format!("{init_text} {drop_body}");
                self.facts.guards.push(Guard {
                    at: loc(node.span()),
                    target_tokens: combined,
                    cleanup_tokens: drop_body,
                });
            }
            if init_text.contains("scopeguard :: guard")
                || init_text.contains("ScopeGuard :: new")
                || init_text.contains("scopeguard :: ScopeGuard :: new")
            {
                self.facts.guards.push(Guard {
                    at: loc(node.span()),
                    target_tokens: init_text.clone(),
                    cleanup_tokens: init_text,
                });
            }
        }
        visit::visit_local(self, node);
    }
    fn visit_stmt(&mut self, node: &'ast Stmt) {
        if let Stmt::Macro(m) = node {
            let path = compact(&tokens(&m.mac.path));
            let body = compact(&tokens(&m.mac.tokens));
            if path.contains("defer") || path.contains("scopeguard") {
                self.facts.guards.push(Guard {
                    at: loc(m.span()),
                    target_tokens: body.clone(),
                    cleanup_tokens: body,
                });
            }
        }
        visit::visit_stmt(self, node);
    }
    fn visit_expr_macro(&mut self, node: &'ast ExprMacro) {
        let path = compact(&tokens(&node.mac.path));
        let body = compact(&tokens(&node.mac.tokens));
        let full = format!("{path} {body}");
        if path.contains("defer") || path.contains("scopeguard") {
            self.facts.guards.push(Guard {
                at: loc(node.span()),
                target_tokens: body.clone(),
                cleanup_tokens: full,
            });
        }
        visit::visit_expr_macro(self, node);
    }
    fn visit_expr_method_call(&mut self, node: &'ast ExprMethodCall) {
        let method = node.method.to_string();
        let receiver_key = state_key(&node.receiver);
        if MUTATING_METHODS.contains(&method.as_str()) {
            if let Some(target) = receiver_key.clone() {
                self.facts.mutations.push(Mutation {
                    at: loc(node.span()),
                    target,
                    kind: method.clone(),
                    status_like: matches!(
                        method.as_str(),
                        "store"
                            | "set"
                            | "fetch_add"
                            | "fetch_sub"
                            | "fetch_or"
                            | "fetch_and"
                            | "fetch_xor"
                    ),
                });
            }
        }
        if let Some(target) = receiver_key {
            self.facts.actions.push(Action {
                at: loc(node.span()),
                target,
                kind: method.clone(),
                text: compact(&tokens(node)),
            });
        } else {
            self.facts.actions.push(Action {
                at: loc(node.span()),
                target: String::new(),
                kind: method.clone(),
                text: compact(&tokens(node)),
            });
        }
        visit::visit_expr_method_call(self, node);
    }
    fn visit_expr_call(&mut self, node: &'ast ExprCall) {
        let f = compact(&tokens(&node.func));
        if f.ends_with("mem :: replace")
            || f.ends_with("mem :: swap")
            || f == "replace"
            || f == "swap"
        {
            if let Some(arg) = node.args.first() {
                if let Some(target) = state_key(arg) {
                    self.facts.mutations.push(Mutation {
                        at: loc(node.span()),
                        target,
                        kind: f.clone(),
                        status_like: false,
                    });
                }
            }
        }
        if f.to_lowercase().contains("guard") || f.contains("scopeguard") {
            let body = compact(&tokens(node));
            self.facts.guards.push(Guard {
                at: loc(node.span()),
                target_tokens: body.clone(),
                cleanup_tokens: body,
            });
        }
        self.facts.actions.push(Action {
            at: loc(node.span()),
            target: String::new(),
            kind: f,
            text: compact(&tokens(node)),
        });
        visit::visit_expr_call(self, node);
    }
    fn visit_expr_assign(&mut self, node: &'ast ExprAssign) {
        if let Some(target) = state_key(&node.left) {
            let rhs = compact(&tokens(&node.right));
            let status_like = rhs == "true"
                || rhs.ends_with(" :: Done")
                || rhs.ends_with(" :: Complete")
                || rhs.ends_with(" :: Finished")
                || rhs.ends_with(" :: Committed")
                || rhs.ends_with(" :: Closed");
            self.facts.mutations.push(Mutation {
                at: loc(node.span()),
                target: target.clone(),
                kind: "assign".into(),
                status_like,
            });
            self.facts.actions.push(Action {
                at: loc(node.span()),
                target,
                kind: "assign".into(),
                text: compact(&tokens(node)),
            });
        }
        visit::visit_expr_assign(self, node);
    }
    fn visit_expr_binary(&mut self, node: &'ast ExprBinary) {
        if matches!(
            node.op,
            syn::BinOp::AddAssign(_)
                | syn::BinOp::SubAssign(_)
                | syn::BinOp::MulAssign(_)
                | syn::BinOp::DivAssign(_)
                | syn::BinOp::BitAndAssign(_)
                | syn::BinOp::BitOrAssign(_)
                | syn::BinOp::BitXorAssign(_)
                | syn::BinOp::ShlAssign(_)
                | syn::BinOp::ShrAssign(_)
        ) {
            if let Some(target) = state_key(&node.left) {
                self.facts.mutations.push(Mutation {
                    at: loc(node.span()),
                    target: target.clone(),
                    kind: "compound_assign".into(),
                    status_like: true,
                });
                self.facts.actions.push(Action {
                    at: loc(node.span()),
                    target,
                    kind: "compound_assign".into(),
                    text: compact(&tokens(node)),
                });
            }
        }
        visit::visit_expr_binary(self, node);
    }
    fn visit_expr_unary(&mut self, node: &'ast ExprUnary) {
        visit::visit_expr_unary(self, node);
    }
}

fn expr_mentions_terminal(text: &str) -> bool {
    TERMINAL_METHODS.iter().any(|m| text.contains(m))
}
fn is_restore_action(action: &Action) -> bool {
    RESTORE_METHODS.contains(&action.kind.as_str()) || action.kind == "assign"
}
fn target_mentions(tokens: &str, target: &str) -> bool {
    if target.is_empty() {
        return false;
    }
    let parts = target.split('.').filter(|x| !x.is_empty());
    parts.into_iter().any(|part| tokens.contains(part))
}
fn guard_is_effective(guard: &Guard, mutation: &Mutation, await_at: LineColumn) -> bool {
    if !before(guard.at, await_at) {
        return false;
    }
    let mentions = target_mentions(&guard.target_tokens, &mutation.target);
    let cleanup = [&guard.cleanup_tokens].iter().any(|s| {
        [
            "restore", "insert", "push", "replace", "put", "reinsert", "rollback", "take", "Some",
            "=",
        ]
        .iter()
        .any(|word| s.contains(word))
    });
    mentions && cleanup
}

fn analyze_facts(mut facts: ScopeFacts, path: &str, scope: &str) -> Vec<Hit> {
    facts.mutations.sort_by_key(|m| (m.at.line, m.at.column));
    facts.awaits.sort_by_key(|a| (a.at.line, a.at.column));
    facts.actions.sort_by_key(|a| (a.at.line, a.at.column));
    for mutation in &facts.mutations {
        if !facts.shared.contains(&root_from_key(&mutation.target)) {
            continue;
        }
        for await_point in facts.awaits.iter().filter(|a| after(a.at, mutation.at)) {
            if facts
                .guards
                .iter()
                .any(|g| guard_is_effective(g, mutation, await_point.at))
            {
                continue;
            }
            let recovered = facts.actions.iter().find(|a| {
                after(a.at, await_point.at) && a.target == mutation.target && is_restore_action(a)
            });
            let terminal_commit = mutation.status_like
                && (expr_mentions_terminal(&await_point.text)
                    || facts
                        .actions
                        .iter()
                        .any(|a| after(a.at, await_point.at) && expr_mentions_terminal(&a.text)));
            if let Some(action) = recovered {
                facts.candidates.push(Hit {
                    path: path.to_string(),
                    line: mutation.at.line,
                    scope: scope.to_string(),
                    mutation: mutation.kind.clone(),
                    target: mutation.target.clone(),
                    await_line: await_point.at.line,
                    recovery: format!("{}@{}", action.kind, action.at.line),
                });
                break;
            } else if terminal_commit {
                facts.candidates.push(Hit {
                    path: path.to_string(),
                    line: mutation.at.line,
                    scope: scope.to_string(),
                    mutation: mutation.kind.clone(),
                    target: mutation.target.clone(),
                    await_line: await_point.at.line,
                    recovery: "terminal_commit_after_await".into(),
                });
                break;
            }
        }
    }
    facts.candidates.sort_by_key(|h| (h.line, h.await_line));
    facts
        .candidates
        .dedup_by(|a, b| a.path == b.path && a.line == b.line && a.scope == b.scope);
    facts.candidates
}
fn root_from_key(key: &str) -> String {
    key.split('.').next().unwrap_or(key).trim().to_string()
}

fn analyze_expr_body(
    expr: &Expr,
    path: &str,
    scope: &str,
    shared: HashSet<String>,
    drop_impls: HashMap<String, String>,
) -> Vec<Hit> {
    let mut facts = ScopeFacts {
        shared,
        drop_impls,
        ..Default::default()
    };
    FactsCollector { facts: &mut facts }.visit_expr(expr);
    analyze_facts(facts, path, scope)
}
fn analyze_block(
    block: &syn::Block,
    path: &str,
    scope: &str,
    shared: HashSet<String>,
    drop_impls: HashMap<String, String>,
) -> Vec<Hit> {
    let mut facts = ScopeFacts {
        shared,
        drop_impls,
        ..Default::default()
    };
    FactsCollector { facts: &mut facts }.visit_block(block);
    analyze_facts(facts, path, scope)
}
fn shared_fn_inputs(sig: &syn::Signature) -> HashSet<String> {
    let mut shared = HashSet::new();
    for input in &sig.inputs {
        match input {
            syn::FnArg::Receiver(_) => {
                shared.insert("self".into());
            }
            syn::FnArg::Typed(arg) if is_shared_type(&arg.ty) => {
                if let Some(name) = ident_from_pat(&arg.pat) {
                    shared.insert(name);
                }
            }
            _ => {}
        }
    }
    shared
}

#[derive(Default)]
struct SharedLocalCollector {
    names: HashSet<String>,
}
impl<'ast> Visit<'ast> for SharedLocalCollector {
    fn visit_expr_async(&mut self, _: &'ast ExprAsync) { /* nested scopes are analyzed independently */
    }
    fn visit_expr_closure(&mut self, node: &'ast ExprClosure) {
        if node.asyncness.is_some() {
            return;
        }
        visit::visit_expr_closure(self, node);
    }
    fn visit_local(&mut self, node: &'ast Local) {
        if let (Some(name), Pat::Type(p)) = (ident_from_pat(&node.pat), &node.pat) {
            if is_shared_type(&p.ty) {
                self.names.insert(name);
            }
        }
        visit::visit_local(self, node);
    }
}
fn scope_shared(sig: &syn::Signature, block: &syn::Block) -> HashSet<String> {
    let mut shared = shared_fn_inputs(sig);
    let mut locals = SharedLocalCollector::default();
    locals.visit_block(block);
    shared.extend(locals.names);
    shared
}

struct ScopeFinder<'a> {
    path: &'a str,
    hits: Vec<Hit>,
    drop_impls: HashMap<String, String>,
    context_shared: HashSet<String>,
}
impl<'ast> Visit<'ast> for ScopeFinder<'_> {
    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        let previous = self.context_shared.clone();
        let shared = scope_shared(&node.sig, &node.block);
        if node.sig.asyncness.is_some() {
            self.hits.extend(analyze_block(
                &node.block,
                self.path,
                &node.sig.ident.to_string(),
                shared.clone(),
                self.drop_impls.clone(),
            ));
        }
        self.context_shared = shared;
        visit::visit_item_fn(self, node);
        self.context_shared = previous;
    }
    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        let previous = self.context_shared.clone();
        let shared = scope_shared(&node.sig, &node.block);
        if node.sig.asyncness.is_some() {
            self.hits.extend(analyze_block(
                &node.block,
                self.path,
                &node.sig.ident.to_string(),
                shared.clone(),
                self.drop_impls.clone(),
            ));
        }
        self.context_shared = shared;
        visit::visit_impl_item_fn(self, node);
        self.context_shared = previous;
    }
    fn visit_trait_item_fn(&mut self, node: &'ast syn::TraitItemFn) {
        let previous = self.context_shared.clone();
        let shared = if let Some(block) = &node.default {
            scope_shared(&node.sig, block)
        } else {
            shared_fn_inputs(&node.sig)
        };
        if node.sig.asyncness.is_some() {
            if let Some(block) = &node.default {
                self.hits.extend(analyze_block(
                    block,
                    self.path,
                    &node.sig.ident.to_string(),
                    shared.clone(),
                    self.drop_impls.clone(),
                ));
            }
        }
        self.context_shared = shared;
        visit::visit_trait_item_fn(self, node);
        self.context_shared = previous;
    }
    fn visit_expr_async(&mut self, node: &'ast ExprAsync) {
        let scope = format!("async@{}", line_of(node.span()));
        let mut shared = self.context_shared.clone();
        shared.insert("self".into());
        let mut nested_locals = SharedLocalCollector::default();
        nested_locals.visit_block(&node.block);
        shared.extend(nested_locals.names);
        self.hits.extend(analyze_block(
            &node.block,
            self.path,
            &scope,
            shared.clone(),
            self.drop_impls.clone(),
        ));
        let previous = std::mem::replace(&mut self.context_shared, shared);
        visit::visit_expr_async(self, node);
        self.context_shared = previous;
    }
    fn visit_expr_closure(&mut self, node: &'ast ExprClosure) {
        if node.asyncness.is_some() {
            let scope = format!("async_closure@{}", line_of(node.span()));
            let mut shared = self.context_shared.clone();
            for input in &node.inputs {
                if let Pat::Type(p) = input {
                    if is_shared_type(&p.ty) {
                        if let Some(name) = ident_from_pat(input) {
                            shared.insert(name);
                        }
                    }
                }
            }
            self.hits.extend(analyze_expr_body(
                &node.body,
                self.path,
                &scope,
                shared,
                self.drop_impls.clone(),
            ));
        }
        visit::visit_expr_closure(self, node);
    }
}

#[derive(Default)]
struct DropImplCollector {
    bodies: HashMap<String, String>,
}
impl<'ast> Visit<'ast> for DropImplCollector {
    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        let is_drop = node
            .trait_
            .as_ref()
            .map(|(_, path, _)| {
                path.segments
                    .last()
                    .map(|s| s.ident == "Drop")
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        if is_drop {
            if let syn::Type::Path(ty) = &*node.self_ty {
                if let Some(name) = ty.path.segments.last().map(|s| s.ident.to_string()) {
                    self.bodies.insert(name, compact(&tokens(node)));
                }
            }
        }
        visit::visit_item_impl(self, node);
    }
}

fn scan_file(path: &Path) -> Result<Vec<Hit>, String> {
    let src = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let ast = syn::parse_file(&src).map_err(|e| format!("{}: {e}", path.display()))?;
    let display = path.to_string_lossy().replace('\\', "/");
    let mut drop_collector = DropImplCollector::default();
    drop_collector.visit_file(&ast);
    let mut finder = ScopeFinder {
        path: &display,
        hits: Vec::new(),
        drop_impls: drop_collector.bodies,
        context_shared: HashSet::new(),
    };
    finder.visit_file(&ast);
    Ok(finder.hits)
}

fn read_files(args: &[String]) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--files-from" {
            i += 1;
            let list = args.get(i).ok_or("--files-from requires a file")?;
            let contents = fs::read_to_string(list).map_err(|e| format!("{list}: {e}"))?;
            files.extend(
                contents
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .map(PathBuf::from),
            );
        } else if args[i] == "--help" || args[i] == "-h" {
            println!("Usage: phase3-cancel-state-detector [--files-from FILE] [RUST_FILE ...]\nReads Rust files and prints function-level candidates as TSV.");
            return Ok(Vec::new());
        } else {
            files.push(PathBuf::from(&args[i]));
        }
        i += 1;
    }
    Ok(files)
}
fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let files = match read_files(&args) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    println!("file\tline\tscope\tmutation\ttarget\tawait_line\trecovery");
    let mut errors = Vec::new();
    for file in files {
        match scan_file(&file) {
            Ok(hits) => {
                for h in hits {
                    println!(
                        "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                        h.path,
                        h.line,
                        h.scope,
                        h.mutation,
                        h.target.replace('\t', " "),
                        h.await_line,
                        h.recovery
                    );
                }
            }
            Err(e) => errors.push(e),
        }
    }
    for e in &errors {
        eprintln!("PARSE_ERROR\t{e}");
    }
    if !errors.is_empty() {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scan(source: &str) -> Vec<Hit> {
        let ast = syn::parse_file(source).expect("test Rust parses");
        let mut finder = ScopeFinder {
            path: "test.rs",
            hits: Vec::new(),
            drop_impls: {
                let mut d = DropImplCollector::default();
                d.visit_file(&ast);
                d.bodies
            },
            context_shared: HashSet::new(),
        };
        finder.visit_file(&ast);
        finder.hits
    }

    #[test]
    fn finds_take_restore_across_await_on_borrowed_shared_state() {
        let hits = scan(
            r#"
            async fn run(slot: &mut Option<u8>) {
                let item = slot.take();
                work().await;
                *slot = item;
            }
        "#,
        );
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].mutation, "take");
        assert_eq!(hits[0].await_line, 4);
    }

    #[test]
    fn finds_premature_completion_flag_before_terminal_await() {
        let hits = scan(
            r#"
            struct S { finished: bool }
            impl S {
                async fn run(&mut self) {
                    self.finished = true;
                    self.on_finish().await;
                }
                async fn on_finish(&self) {}
            }
        "#,
        );
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].recovery, "terminal_commit_after_await");
    }

    #[test]
    fn ignores_owned_local_state_without_shared_origin() {
        let hits = scan(
            r#"
            async fn run() {
                let mut local = vec![1];
                let value = local.pop();
                work().await;
                local.push(value.unwrap());
            }
        "#,
        );
        assert!(hits.is_empty());
    }

    #[test]
    fn ignores_mutation_after_await_or_without_await() {
        let hits = scan(
            r#"
            async fn run(state: &mut Vec<u8>) {
                work().await;
                state.pop();
            }
            async fn other(state: &mut Vec<u8>) { state.pop(); }
        "#,
        );
        assert!(hits.is_empty());
    }

    #[test]
    fn recognizes_visible_defer_rollback_as_protection() {
        let hits = scan(
            r#"
            async fn run(slot: &mut Option<u8>) {
                let item = slot.take();
                scopeguard::defer! { *slot = item; }
                work().await;
                *slot = None;
            }
        "#,
        );
        assert!(hits.is_empty());
    }

    #[test]
    fn recognizes_drop_guard_rollback_as_protection() {
        let hits = scan(
            r#"
            struct Rollback<'a> { slot: &'a mut Option<u8>, item: Option<u8> }
            impl Drop for Rollback<'_> {
                fn drop(&mut self) { *self.slot = self.item.take(); }
            }
            async fn run(slot: &mut Option<u8>) {
                let item = slot.take();
                let _guard = Rollback { slot, item };
                work().await;
                *slot = None;
            }
        "#,
        );
        assert!(hits.is_empty());
    }

    #[test]
    fn analyzes_mutation_in_nested_async_block_capturing_shared_state() {
        let hits = scan(
            r#"
            async fn outer(slot: &mut Option<u8>) {
                async { let item = slot.take(); work().await; *slot = item; }.await;
            }
        "#,
        );
        assert_eq!(hits.len(), 1);
        assert!(hits[0].scope.starts_with("async@"));
    }

    #[test]
    fn analyzes_captured_typed_shared_local_inside_async_block() {
        let hits = scan(
            r#"
            async fn outer() {
                let slot: &mut Option<u8> = todo!();
                async move { let item = slot.take(); work().await; *slot = item; }.await;
            }
        "#,
        );
        assert_eq!(hits.len(), 1);
        assert!(hits[0].scope.starts_with("async@"));
    }
}
