//! Recursive-descent parser for declarations and statements; Pratt parser for
//! expressions.
//!
//! Error recovery is a specified requirement, not best-effort. On an unexpected
//! token the parser reports once, then skips to the next synchronisation point
//! — a statement or declaration boundary at the current brace depth. A missing
//! closing brace produces one diagnostic, not forty.

use kite_ast::*;
use kite_diag::{codes, DiagBag, Diagnostic};
use kite_lexer::{Token, TokenKind as T};
use kite_span::{FileId, Span};

mod prec;
use prec::{infix_binding_power, InfixOp};

/// The index of the `)` closing the `(` at `open`, accounting for nesting and
/// for parens inside nested string literals.
fn matching_paren(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            b'"' => {
                // A nested literal: its own parens are not ours.
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// The type path a struct literal's head spells, if it spells one.
///
/// `Point` arrives as a path and `ui.Style` as a field access, because whether
/// `a.b` names a module or reads a field is a resolution question. Both are
/// runs of plain names, and nothing else can precede a `{`.
fn type_path_of(expr: &Expr) -> Option<TypePath> {
    match expr {
        Expr::Path(p) => Some(TypePath {
            segments: p.segments.clone(),
            args: Vec::new(),
            span: p.span,
        }),
        Expr::Field { base, name, span } => {
            let mut path = type_path_of(base)?;
            path.segments.push(name.clone());
            path.span = *span;
            Some(path)
        }
        _ => None,
    }
}

pub fn parse(file: FileId, src: &str, tokens: &[Token], diags: &mut DiagBag) -> SourceFile {
    let mut p = Parser {
        file,
        src,
        tokens,
        pos: 0,
        diags,
        panicking: false,
        no_struct_literal: 0,
        in_hole: false,
        split_gt: false,
    };
    p.parse_source_file()
}

struct Parser<'a> {
    file: FileId,
    /// The parser reads identifier text back from source, because the lexer
    /// stores none.
    src: &'a str,
    tokens: &'a [Token],
    pos: usize,
    diags: &'a mut DiagBag,
    /// Set after reporting a syntax error, cleared at the next synchronisation
    /// point. Suppresses the cascade of follow-on errors that one bad token
    /// would otherwise produce.
    panicking: bool,
    /// Non-zero while parsing an `if`/`for`/`match` scrutinee, where a `{`
    /// begins the body rather than a struct literal.
    no_struct_literal: u32,
    /// Set for the sub-parser of a `\(...)` hole, where "end of input" means
    /// the closing paren.
    in_hole: bool,
    /// Half of a `>>` has been consumed as the `>` closing a type argument
    /// list; the other half is still to come.
    split_gt: bool,
}

impl<'a> Parser<'a> {
    // ---- cursor -----------------------------------------------------------

    fn peek(&self) -> T {
        // `Box<Box<int>>` ends in a token the lexer read as a shift. Rather
        // than make the lexer care about types, the parser splits it: the
        // first half is consumed as `>` and the second is left standing here.
        if self.split_gt {
            return T::Gt;
        }
        self.tokens[self.pos].kind
    }

    fn peek_at(&self, n: usize) -> T {
        self.tokens
            .get(self.pos + n)
            .map(|t| t.kind)
            .unwrap_or(T::Eof)
    }

    fn span(&self) -> Span {
        self.tokens[self.pos].span
    }

    fn prev_span(&self) -> Span {
        self.tokens[self.pos.saturating_sub(1)].span
    }

    fn at(&self, k: T) -> bool {
        self.peek() == k
    }

    fn at_end(&self) -> bool {
        self.peek() == T::Eof
    }

    fn bump(&mut self) -> Token {
        if self.split_gt {
            self.split_gt = false;
            let t = self.tokens[self.pos];
            self.pos += 1;
            return Token { kind: T::Gt, span: Span::new(t.span.file, t.span.start + 1, t.span.end) };
        }
        let t = self.tokens[self.pos];
        if t.kind != T::Eof {
            self.pos += 1;
        }
        t
    }

    /// Consume the `>` that closes a type argument list, splitting a `>>` if
    /// that is what the lexer produced.
    fn expect_closing_gt(&mut self) -> Option<Span> {
        if self.tokens[self.pos].kind == T::Shr && !self.split_gt {
            let t = self.tokens[self.pos];
            self.split_gt = true;
            return Some(Span::new(t.span.file, t.span.start, t.span.start + 1));
        }
        self.expect(T::Gt)
    }

    fn eat(&mut self, k: T) -> bool {
        if self.at(k) {
            self.bump();
            true
        } else {
            false
        }
    }

    /// Inside any bracketed context a `{` can only be a struct or map literal,
    /// never a body, so the suppression is lifted. This is what makes the
    /// specification's advice work: parenthesise the literal.
    fn in_brackets<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let saved = std::mem::replace(&mut self.no_struct_literal, 0);
        let r = f(self);
        self.no_struct_literal = saved;
        r
    }

    /// Newlines are insignificant here — inside a delimited construct, or
    /// between the statements of a block.
    fn skip_newlines(&mut self) {
        while self.at(T::Newline) {
            self.bump();
        }
    }

    fn expect(&mut self, k: T) -> Option<Span> {
        if self.at(k) {
            return Some(self.bump().span);
        }
        self.error_expected(&format!("`{}`", k.text()));
        None
    }

    fn error_expected(&mut self, what: &str) {
        if self.panicking {
            return;
        }
        self.panicking = true;
        let found = self.peek();
        let span = if found == T::Newline || found == T::Eof {
            // Point at the end of the previous token rather than at an
            // invisible one, so the caret lands where the fix goes.
            let p = self.prev_span();
            Span::empty_at(self.file, p.end)
        } else {
            self.span()
        };
        // Inside a `\(...)` the input ends at the closing paren, not at the
        // end of the file, and saying otherwise sends the reader to the wrong
        // place entirely.
        let described = if self.in_hole && matches!(found, T::Eof | T::Newline) {
            "the end of this interpolation"
        } else {
            found.describe()
        };
        self.diags.push(
            Diagnostic::error(codes::E0100, format!("expected {}", what))
                .with_primary(span, format!("found {}", described)),
        );
    }

    /// Skip forward to somewhere parsing can sensibly resume.
    fn synchronize(&mut self) {
        self.panicking = false;
        let mut depth = 0i32;
        loop {
            match self.peek() {
                T::Eof => return,
                T::Newline if depth <= 0 => {
                    self.bump();
                    return;
                }
                T::RBrace if depth <= 0 => return,
                T::LBrace | T::LParen | T::LBracket => {
                    depth += 1;
                    self.bump();
                }
                T::RBrace | T::RParen | T::RBracket => {
                    depth -= 1;
                    self.bump();
                }
                k if depth <= 0 && k.starts_declaration() => return,
                _ => {
                    self.bump();
                }
            }
        }
    }

    /// Consume the end of a statement.
    fn expect_terminator(&mut self) {
        match self.peek() {
            T::Newline => {
                self.bump();
            }
            T::RBrace | T::Eof => {}
            _ => {
                self.error_expected("a line break");
                self.synchronize();
            }
        }
    }

    fn ident(&mut self) -> Option<Ident> {
        if self.at(T::Ident) {
            let t = self.bump();
            return Some(Ident::new(self.text(t.span), t.span));
        }
        self.error_expected("an identifier");
        None
    }

    /// Source text of a span.
    fn text(&self, span: Span) -> &'a str {
        &self.src[span.start as usize..span.end as usize]
    }

    /// Source text of the token at `pos`.
    fn text_at(&self, pos: usize) -> &'a str {
        match self.tokens.get(pos) {
            Some(t) => self.text(t.span),
            None => "",
        }
    }

    // ---- source file ------------------------------------------------------

    fn parse_source_file(&mut self) -> SourceFile {
        let mut file = SourceFile::default();
        self.skip_newlines();

        while self.at(T::Use) {
            if let Some(u) = self.parse_use() {
                file.uses.push(u);
            }
            self.skip_newlines();
        }

        while !self.at_end() {
            self.skip_newlines();
            if self.at_end() {
                break;
            }
            let before = self.pos;
            match self.parse_item() {
                Some(item) => file.items.push(item),
                None => {
                    let span = self.span();
                    self.synchronize();
                    file.items.push(Item::Error(span));
                }
            }
            // Guarantee forward progress even if a sub-parser returned without
            // consuming, which would otherwise hang the compiler.
            if self.pos == before {
                self.bump();
            }
            self.skip_newlines();
        }
        file
    }

    fn parse_use(&mut self) -> Option<Use> {
        let start = self.span();
        self.bump(); // `use`
        let mut path = vec![self.ident()?];
        while self.eat(T::Slash) {
            path.push(self.ident()?);
        }
        let alias = if self.eat(T::As) { Some(self.ident()?) } else { None };
        let span = start.to(self.prev_span());
        self.expect_terminator();
        Some(Use { path, alias, span })
    }

    fn parse_item(&mut self) -> Option<Item> {
        let start = self.span();
        // `@host("net")` — the only attribute in the language, and the only
        // one planned. It marks the declared boundary with the host, which the
        // glue is generated from.
        let host = if self.at(T::At) { Some(self.parse_host_attribute()?) } else { None };
        let is_pub = self.eat(T::Pub);
        let is_async = self.eat(T::Async);

        if let Some(host) = host {
            return self.parse_extern(is_pub, host, start).map(Item::Extern);
        }

        match self.peek() {
            T::Fn => self.parse_fn(is_pub, is_async, start).map(Item::Fn),
            T::Struct if !is_async => self.parse_struct(is_pub, start).map(Item::Struct),
            T::Enum if !is_async => self.parse_enum(is_pub, start).map(Item::Enum),
            T::Trait if !is_async => self.parse_trait(is_pub, start).map(Item::Trait),
            T::Impl if !is_async => self.parse_impl(start).map(Item::Impl),
            T::Type if !is_async => self.parse_type_alias(is_pub, start).map(Item::TypeAlias),
            _ if is_async => {
                self.error_expected("`fn` after `async`");
                None
            }
            _ => {
                self.error_expected("a declaration");
                None
            }
        }
    }

    /// `<T, U: Bound>`
    fn parse_generics(&mut self) -> Option<Vec<GenericParam>> {
        if !self.at(T::Lt) {
            return Some(Vec::new());
        }
        self.bump();
        let mut params = Vec::new();
        while !self.at(T::Gt) && !self.at_end() {
            let start = self.span();
            let name = self.ident()?;
            let mut bounds = Vec::new();
            if self.eat(T::Colon) {
                bounds.push(self.parse_type_path()?);
                while self.eat(T::Plus) {
                    bounds.push(self.parse_type_path()?);
                }
            }
            params.push(GenericParam { name, bounds, span: start.to(self.prev_span()) });
            if !self.eat(T::Comma) {
                break;
            }
        }
        self.expect(T::Gt)?;
        Some(params)
    }

    fn parse_struct(&mut self, is_pub: bool, start: Span) -> Option<StructDecl> {
        self.bump(); // `struct`
        let name = self.ident()?;
        let generics = self.parse_generics()?;
        self.expect(T::LBrace)?;

        let mut fields = Vec::new();
        loop {
            self.skip_newlines();
            if self.at(T::RBrace) || self.at_end() {
                break;
            }
            let before = self.pos;
            match self.parse_field_decl() {
                Some(f) => fields.push(f),
                None => self.synchronize_in_braces(),
            }
            if self.pos == before {
                self.bump();
            }
        }
        let end = self.expect(T::RBrace)?;
        Some(StructDecl { is_pub, name, generics, fields, span: start.to(end) })
    }

    fn parse_field_decl(&mut self) -> Option<FieldDecl> {
        let start = self.span();
        let is_pub = self.eat(T::Pub);
        let is_var = self.eat(T::Var);
        let name = self.ident()?;
        self.expect(T::Colon)?;
        let ty = self.parse_type()?;
        let span = start.to(self.prev_span());
        self.expect_terminator();
        Some(FieldDecl { is_pub, is_var, name, ty, span })
    }

    fn parse_enum(&mut self, is_pub: bool, start: Span) -> Option<EnumDecl> {
        self.bump(); // `enum`
        let name = self.ident()?;
        let generics = self.parse_generics()?;
        self.expect(T::LBrace)?;

        let mut variants = Vec::new();
        loop {
            self.skip_newlines();
            if self.at(T::RBrace) || self.at_end() {
                break;
            }
            let before = self.pos;
            match self.parse_variant() {
                Some(v) => variants.push(v),
                None => self.synchronize_in_braces(),
            }
            if self.pos == before {
                self.bump();
            }
        }
        let end = self.expect(T::RBrace)?;
        Some(EnumDecl { is_pub, name, generics, variants, span: start.to(end) })
    }

    fn parse_variant(&mut self) -> Option<VariantDecl> {
        let start = self.span();
        let name = self.ident()?;

        let payload = if self.at(T::LParen) {
            self.bump();
            self.skip_newlines();
            // `Circle(radius: float)` is named; `Number(float)` positional.
            // One token of lookahead past the first name decides.
            let named = self.at(T::Ident) && self.peek_at(1) == T::Colon;
            let payload = if named {
                let mut fields = Vec::new();
                while !self.at(T::RParen) && !self.at_end() {
                    let f_start = self.span();
                    let f_name = self.ident()?;
                    self.expect(T::Colon)?;
                    let ty = self.parse_type()?;
                    fields.push(FieldDecl {
                        is_pub: true,
                        is_var: false,
                        name: f_name,
                        ty,
                        span: f_start.to(self.prev_span()),
                    });
                    self.skip_newlines();
                    if !self.eat(T::Comma) {
                        break;
                    }
                    self.skip_newlines();
                }
                VariantPayload::Named(fields)
            } else {
                let mut tys = Vec::new();
                while !self.at(T::RParen) && !self.at_end() {
                    tys.push(self.parse_type()?);
                    self.skip_newlines();
                    if !self.eat(T::Comma) {
                        break;
                    }
                    self.skip_newlines();
                }
                VariantPayload::Positional(tys)
            };
            self.expect(T::RParen)?;
            payload
        } else {
            VariantPayload::Unit
        };

        let span = start.to(self.prev_span());
        self.expect_terminator();
        Some(VariantDecl { name, payload, span })
    }

    fn parse_trait(&mut self, is_pub: bool, start: Span) -> Option<TraitDecl> {
        self.bump(); // `trait`
        let name = self.ident()?;
        let generics = self.parse_generics()?;
        self.expect(T::LBrace)?;
        let methods = self.parse_method_list()?;
        let end = self.expect(T::RBrace)?;
        Some(TraitDecl { is_pub, name, generics, methods, span: start.to(end) })
    }

    fn parse_impl(&mut self, start: Span) -> Option<ImplDecl> {
        self.bump(); // `impl`
        let generics = self.parse_generics()?;
        let first = self.parse_type_path()?;

        // `impl Trait for Type` versus an inherent `impl Type`.
        let (trait_path, self_ty) = if self.eat(T::For) {
            let target = self.parse_type_path()?;
            (Some(first), target)
        } else {
            (None, first)
        };

        self.expect(T::LBrace)?;
        let methods = self.parse_method_list()?;
        let end = self.expect(T::RBrace)?;
        Some(ImplDecl { generics, trait_path, self_ty, methods, span: start.to(end) })
    }

    fn parse_method_list(&mut self) -> Option<Vec<MethodDecl>> {
        let mut methods = Vec::new();
        loop {
            self.skip_newlines();
            if self.at(T::RBrace) || self.at_end() {
                break;
            }
            let before = self.pos;
            match self.parse_method() {
                Some(m) => methods.push(m),
                None => self.synchronize_in_braces(),
            }
            if self.pos == before {
                self.bump();
            }
        }
        Some(methods)
    }

    fn parse_method(&mut self) -> Option<MethodDecl> {
        let start = self.span();
        let is_pub = self.eat(T::Pub);
        let is_async = self.eat(T::Async);
        self.expect(T::Fn)?;
        let name = self.ident()?;
        // A method's own `<T>` list is parsed and discarded: type parameters
        // live on the declaration a method belongs to, not on the method.
        let _generics = self.parse_generics()?;

        self.expect(T::LParen)?;
        self.skip_newlines();

        // A receiver, if present, is the first parameter.
        let self_param = if self.at(T::SelfKw) || (self.at(T::Var) && self.peek_at(1) == T::SelfKw)
        {
            let s_start = self.span();
            let is_var = self.eat(T::Var);
            self.bump(); // `self`
            Some(SelfParam { is_var, span: s_start.to(self.prev_span()) })
        } else {
            None
        };
        if self_param.is_some() && !self.at(T::RParen) {
            self.expect(T::Comma)?;
        }

        let mut params = Vec::new();
        self.skip_newlines();
        while !self.at(T::RParen) && !self.at_end() {
            let p_start = self.span();
            let is_var = self.eat(T::Var);
            let p_name = self.ident()?;
            self.expect(T::Colon)?;
            let ty = self.parse_type()?;
            params.push(Param { is_var, name: p_name, ty, span: p_start.to(self.prev_span()) });
            self.skip_newlines();
            if !self.eat(T::Comma) {
                break;
            }
            self.skip_newlines();
        }
        self.expect(T::RParen)?;

        let ret = if self.eat(T::Arrow) {
            Some(self.parse_return_type()?)
        } else {
            None
        };
        let sig_span = start.to(self.prev_span());

        // A trait method may declare no body.
        let body = if self.at(T::LBrace) {
            Some(self.parse_block()?)
        } else {
            self.expect_terminator();
            None
        };

        let span = start.to(self.prev_span());
        Some(MethodDecl {
            is_pub,
            is_async,
            name,
            self_param,
            params,
            ret,
            body,
            span,
            sig_span,
        })
    }

    fn parse_type_alias(&mut self, is_pub: bool, start: Span) -> Option<TypeAlias> {
        self.bump(); // `type`
        let name = self.ident()?;
        let generics = self.parse_generics()?;
        self.expect(T::Eq)?;
        let ty = self.parse_type()?;
        let span = start.to(self.prev_span());
        self.expect_terminator();
        Some(TypeAlias { is_pub, name, generics, ty, span })
    }

    /// Recover to the next member boundary inside a braced declaration body,
    /// without consuming the closing brace.
    fn synchronize_in_braces(&mut self) {
        self.panicking = false;
        let mut depth = 0i32;
        loop {
            match self.peek() {
                T::Eof => return,
                T::RBrace if depth <= 0 => return,
                T::Newline if depth <= 0 => {
                    self.bump();
                    return;
                }
                T::LBrace | T::LParen | T::LBracket => {
                    depth += 1;
                    self.bump();
                }
                T::RBrace | T::RParen | T::RBracket => {
                    depth -= 1;
                    self.bump();
                }
                _ => {
                    self.bump();
                }
            }
        }
    }

    /// `@host("net")` — the group a host declaration belongs to.
    fn parse_host_attribute(&mut self) -> Option<String> {
        self.bump(); // `@`
        let name = self.ident()?;
        if name.name != "host" {
            self.diags.push(
                Diagnostic::error(
                    codes::E0100,
                    format!("unknown attribute `@{}`", name.name),
                )
                .with_primary(name.span, "not an attribute")
                .with_note("`@host(\"group\")` is the only attribute Kite has"),
            );
            return None;
        }
        self.expect(T::LParen)?;
        let span = self.span();
        if !self.at(T::Str) {
            self.error_expected("a string naming the host group");
            return None;
        }
        self.bump();
        self.expect(T::RParen)?;
        self.skip_newlines();
        // The quotes are part of the span; the group is what is between them.
        let text = self.text(span);
        Some(text.trim_matches('"').to_string())
    }

    /// `@host("net") extern fn fetch(url: str) -> int`
    fn parse_extern(&mut self, is_pub: bool, host: String, start: Span) -> Option<ExternDecl> {
        if !self.at(T::Ident) || self.text(self.span()) != "extern" {
            self.error_expected("`extern` after a `@host` attribute");
            return None;
        }
        self.bump(); // `extern`
        if !self.at(T::Fn) {
            self.error_expected("`fn` after `extern`");
            return None;
        }
        self.bump(); // `fn`
        let name = self.ident()?;

        self.expect(T::LParen)?;
        let mut params = Vec::new();
        self.skip_newlines();
        while !self.at(T::RParen) && !self.at_end() {
            let p_start = self.span();
            let p_name = self.ident()?;
            self.expect(T::Colon)?;
            let ty = self.parse_type()?;
            params.push(Param { is_var: false, name: p_name, ty, span: p_start.to(self.prev_span()) });
            self.skip_newlines();
            if !self.eat(T::Comma) {
                break;
            }
            self.skip_newlines();
        }
        self.expect(T::RParen)?;

        let ret = if self.eat(T::Arrow) {
            Some(self.parse_return_type()?)
        } else {
            None
        };
        let span = start.to(self.prev_span());
        Some(ExternDecl { is_pub, host, name, params, ret, span, sig_span: span })
    }

    fn parse_fn(&mut self, is_pub: bool, is_async: bool, start: Span) -> Option<FnDecl> {
        self.bump(); // `fn`
        let name = self.ident()?;
        let generics = self.parse_generics()?;

        self.expect(T::LParen)?;
        let mut params = Vec::new();
        self.skip_newlines();
        while !self.at(T::RParen) && !self.at_end() {
            let p_start = self.span();
            let is_var = self.eat(T::Var);
            let p_name = self.ident()?;
            self.expect(T::Colon)?;
            let ty = self.parse_type()?;
            params.push(Param {
                is_var,
                name: p_name,
                ty,
                span: p_start.to(self.prev_span()),
            });
            self.skip_newlines();
            if !self.eat(T::Comma) {
                break;
            }
            self.skip_newlines();
        }
        self.expect(T::RParen)?;

        let ret = if self.eat(T::Arrow) {
            Some(self.parse_return_type()?)
        } else {
            None
        };

        let sig_span = start.to(self.prev_span());
        let body = self.parse_block()?;
        let span = start.to(body.span);

        Some(FnDecl { is_pub, is_async, name, generics, params, ret, body, span, sig_span })
    }

    /// `-> T` or `-> (T, error)`.
    fn parse_return_type(&mut self) -> Option<RetType> {
        if self.at(T::LParen) {
            // Could be a fallible pair or an ordinary tuple type. It is
            // fallible exactly when the last element is the name `error`.
            let start = self.span();
            let save = self.pos;
            self.bump();
            let first = self.parse_type()?;
            if self.eat(T::Comma)
                && self.at(T::Ident) && self.text_at(self.pos) == "error" {
                    self.bump();
                    let end = self.expect(T::RParen)?;
                    return Some(RetType::Fallible { value: first, span: start.to(end) });
                }
            self.pos = save;
        }
        Some(RetType::Simple(self.parse_type()?))
    }

    // ---- types ------------------------------------------------------------

    fn parse_type(&mut self) -> Option<Type> {
        let start = self.span();
        match self.peek() {
            T::LBracket => {
                self.bump();
                let elem = self.parse_type()?;
                let end = self.expect(T::RBracket)?;
                Some(Type::Slice { elem: Box::new(elem), span: start.to(end) })
            }
            T::LBrace => {
                self.bump();
                let key = self.parse_type()?;
                self.expect(T::Colon)?;
                let value = self.parse_type()?;
                let end = self.expect(T::RBrace)?;
                Some(Type::Map {
                    key: Box::new(key),
                    value: Box::new(value),
                    span: start.to(end),
                })
            }
            T::LParen => {
                self.bump();
                let mut elems = Vec::new();
                self.skip_newlines();
                while !self.at(T::RParen) && !self.at_end() {
                    elems.push(self.parse_type()?);
                    self.skip_newlines();
                    if !self.eat(T::Comma) {
                        break;
                    }
                    self.skip_newlines();
                }
                let end = self.expect(T::RParen)?;
                Some(Type::Tuple { elems, span: start.to(end) })
            }
            T::Fn => {
                self.bump();
                self.expect(T::LParen)?;
                let mut params = Vec::new();
                while !self.at(T::RParen) && !self.at_end() {
                    params.push(self.parse_type()?);
                    if !self.eat(T::Comma) {
                        break;
                    }
                }
                self.expect(T::RParen)?;
                let ret = if self.eat(T::Arrow) {
                    Some(Box::new(self.parse_type()?))
                } else {
                    None
                };
                Some(Type::Fn { params, ret, span: start.to(self.prev_span()) })
            }
            // `Option<T>` is the only built-in generic spelling for now, and
            // the language has no `?` sigil at all.
            T::Ident if self.text_at(self.pos) == "Option" && self.peek_at(1) == T::Lt => {
                self.bump();
                self.bump();
                let inner = self.parse_type()?;
                let end = self.expect_closing_gt()?;
                Some(Type::Optional { inner: Box::new(inner), span: start.to(end) })
            }
            T::Ident if self.text_at(self.pos) == "dyn" => {
                self.bump();
                let path = self.parse_type_path()?;
                Some(Type::Dyn { span: start.to(path.span), path })
            }
            T::Ident => Some(Type::Path(self.parse_type_path()?)),
            _ => {
                self.error_expected("a type");
                None
            }
        }
    }

    fn parse_type_path(&mut self) -> Option<TypePath> {
        let start = self.span();
        let mut segments = vec![self.ident()?];
        while self.at(T::Dot) && self.peek_at(1) == T::Ident {
            self.bump();
            segments.push(self.ident()?);
        }
        let mut args = Vec::new();
        if self.at(T::Lt) {
            self.bump();
            while !self.at(T::Gt) && !self.at_end() {
                args.push(self.parse_type()?);
                if !self.eat(T::Comma) {
                    break;
                }
            }
            self.expect_closing_gt()?;
        }
        Some(TypePath { segments, args, span: start.to(self.prev_span()) })
    }

    // ---- blocks and statements -------------------------------------------

    fn parse_block(&mut self) -> Option<Block> {
        let start = self.span();
        if !self.at(T::LBrace) {
            self.error_expected("`{`");
            return None;
        }
        self.bump();

        let mut stmts = Vec::new();
        loop {
            self.skip_newlines();
            if self.at(T::RBrace) || self.at_end() {
                break;
            }
            let before = self.pos;
            match self.parse_stmt() {
                Some(s) => stmts.push(s),
                None => {
                    let span = self.span();
                    self.synchronize();
                    stmts.push(Stmt::Error(span));
                }
            }
            if self.pos == before {
                self.bump();
            }
        }

        if self.at_end() {
            self.diags.push(
                Diagnostic::error(codes::E0101, "unclosed delimiter")
                    .with_primary(start, "this `{` is never closed")
                    .with_secondary(self.span(), "file ends here"),
            );
            return Some(Block { stmts, span: start.to(self.prev_span()) });
        }
        let end = self.expect(T::RBrace)?;
        Some(Block { stmts, span: start.to(end) })
    }

    fn parse_stmt(&mut self) -> Option<Stmt> {
        let start = self.span();
        match self.peek() {
            T::Let => self.parse_let(start),
            T::Var => self.parse_var(start),
            T::Return => self.parse_return(start),
            T::If => Some(Stmt::If(self.parse_if()?)),
            T::For => Some(Stmt::For(self.parse_for(None, start)?)),
            T::Match => {
                let m = self.parse_match()?;
                Some(Stmt::Match(m))
            }
            T::Check => {
                self.bump();
                let expr = self.parse_expr()?;
                let span = start.to(expr.span());
                self.expect_terminator();
                Some(Stmt::Check { expr, span })
            }
            T::Defer => {
                self.bump();
                let expr = self.parse_expr()?;
                let span = start.to(expr.span());
                self.expect_terminator();
                Some(Stmt::Defer { expr, span })
            }
            T::Break | T::Continue => {
                let is_break = self.at(T::Break);
                self.bump();
                let label = if self.at(T::Ident) {
                    Some(self.ident()?)
                } else {
                    None
                };
                let span = start.to(self.prev_span());
                self.expect_terminator();
                Some(if is_break {
                    Stmt::Break { label, span }
                } else {
                    Stmt::Continue { label, span }
                })
            }
            // `outer: for ...`
            T::Ident if self.peek_at(1) == T::Colon && self.peek_at(2) == T::For => {
                let label = self.ident()?;
                self.bump(); // `:`
                Some(Stmt::For(self.parse_for(Some(label), start)?))
            }
            _ => self.parse_expr_or_assign(start),
        }
    }

    fn parse_let(&mut self, start: Span) -> Option<Stmt> {
        self.bump();
        let binding = self.parse_binding()?;
        let ty = if self.eat(T::Colon) {
            Some(self.parse_type()?)
        } else {
            None
        };
        let init = if self.eat(T::Eq) {
            Some(self.parse_expr()?)
        } else {
            None
        };
        let span = start.to(self.prev_span());
        self.expect_terminator();
        Some(Stmt::Let(LetStmt { binding, ty, init, span }))
    }

    fn parse_var(&mut self, start: Span) -> Option<Stmt> {
        self.bump();
        let name = self.ident()?;
        let ty = if self.eat(T::Colon) {
            Some(self.parse_type()?)
        } else {
            None
        };
        // Unlike `let`, a `var` must be initialised: there is no definite
        // assignment story for something that may be reassigned anyway.
        if !self.eat(T::Eq) {
            self.error_expected("`=`");
            self.diags.push(
                Diagnostic::error(codes::E0110, "`var` must be initialised")
                    .with_primary(name.span, "declared here without a value")
                    .with_note("`let x: T` may be assigned later; `var` may not"),
            );
            return None;
        }
        let init = self.parse_expr()?;
        let span = start.to(init.span());
        self.expect_terminator();
        Some(Stmt::Var(VarStmt { name, ty, init, span }))
    }

    fn parse_binding(&mut self) -> Option<Binding> {
        if self.at(T::LParen) {
            let start = self.span();
            self.bump();
            let mut elems = Vec::new();
            while !self.at(T::RParen) && !self.at_end() {
                if self.at(T::Underscore) {
                    elems.push(BindElem::Wildcard(self.bump().span));
                } else {
                    elems.push(BindElem::Name(self.ident()?));
                }
                if !self.eat(T::Comma) {
                    break;
                }
            }
            let end = self.expect(T::RParen)?;
            return Some(Binding::Tuple { elems, span: start.to(end) });
        }
        Some(Binding::Name(self.ident()?))
    }

    fn parse_return(&mut self, start: Span) -> Option<Stmt> {
        self.bump();

        if matches!(self.peek(), T::Newline | T::RBrace | T::Eof) {
            let span = start.to(self.prev_span());
            self.expect_terminator();
            return Some(Stmt::Return(ReturnStmt { value: None, span }));
        }

        // `return _, err` — the failure arm.
        if self.at(T::Underscore) && self.peek_at(1) == T::Comma {
            self.bump();
            self.bump();
            let error = self.parse_expr()?;
            let span = start.to(error.span());
            self.expect_terminator();
            return Some(Stmt::Return(ReturnStmt {
                value: Some(ReturnValue::Fail { error, span }),
                span,
            }));
        }

        let first = self.parse_expr()?;
        if self.eat(T::Comma) {
            let error = self.parse_expr()?;
            let span = start.to(error.span());
            self.expect_terminator();
            return Some(Stmt::Return(ReturnStmt {
                value: Some(ReturnValue::Pair { value: first, error, span }),
                span,
            }));
        }

        let span = start.to(first.span());
        self.expect_terminator();
        Some(Stmt::Return(ReturnStmt {
            value: Some(ReturnValue::Single(first)),
            span,
        }))
    }

    fn parse_if(&mut self) -> Option<IfStmt> {
        let start = self.span();
        self.bump(); // `if`

        self.no_struct_literal += 1;
        let cond = self.parse_expr();
        self.no_struct_literal -= 1;
        let cond = cond?;

        let then = self.parse_block()?;

        let else_ = if self.at(T::Else) {
            self.bump();
            if self.at(T::If) {
                Some(Box::new(ElseBranch::If(self.parse_if()?)))
            } else {
                Some(Box::new(ElseBranch::Block(self.parse_block()?)))
            }
        } else {
            None
        };

        let span = start.to(self.prev_span());
        Some(IfStmt { cond, then, else_, span })
    }

    fn parse_for(&mut self, label: Option<Ident>, start: Span) -> Option<ForStmt> {
        self.bump(); // `for`

        let header = if self.at(T::LBrace) {
            ForHeader::Loop
        } else if self.looks_like_for_in() {
            let binding = self.parse_binding()?;
            self.expect(T::In)?;
            self.no_struct_literal += 1;
            let iter = self.parse_expr();
            self.no_struct_literal -= 1;
            ForHeader::In { binding, iter: iter? }
        } else {
            self.no_struct_literal += 1;
            let cond = self.parse_expr();
            self.no_struct_literal -= 1;
            ForHeader::While(cond?)
        };

        let body = self.parse_block()?;
        let span = start.to(body.span);
        Some(ForStmt { label, header, body, span })
    }

    /// Token-level lookahead for `for <binding> in`. Avoids backtracking, which
    /// would risk emitting diagnostics for a path we then abandon.
    fn looks_like_for_in(&self) -> bool {
        match self.peek() {
            T::Ident => self.peek_at(1) == T::In,
            T::LParen => {
                let mut i = 1;
                let mut depth = 1;
                while depth > 0 {
                    match self.peek_at(i) {
                        T::Eof => return false,
                        T::LParen => depth += 1,
                        T::RParen => depth -= 1,
                        _ => {}
                    }
                    i += 1;
                }
                self.peek_at(i) == T::In
            }
            _ => false,
        }
    }

    fn parse_match(&mut self) -> Option<MatchExpr> {
        let start = self.span();
        self.bump(); // `match`

        self.no_struct_literal += 1;
        let scrutinee = self.parse_expr();
        self.no_struct_literal -= 1;
        let scrutinee = scrutinee?;

        self.expect(T::LBrace)?;
        let mut arms = Vec::new();
        loop {
            self.skip_newlines();
            if self.at(T::RBrace) || self.at_end() {
                break;
            }
            let before = self.pos;
            match self.parse_match_arm() {
                Some(a) => arms.push(a),
                None => self.synchronize_in_braces(),
            }
            if self.pos == before {
                self.bump();
            }
        }
        let end = self.expect(T::RBrace)?;

        Some(MatchExpr {
            scrutinee: Box::new(scrutinee),
            arms,
            span: start.to(end),
        })
    }

    fn parse_match_arm(&mut self) -> Option<MatchArm> {
        let start = self.span();
        let pattern = self.parse_pattern()?;

        // `Rect(w, h) if w == h => ...`
        let guard = if self.eat(T::If) {
            self.no_struct_literal += 1;
            let g = self.parse_expr();
            self.no_struct_literal -= 1;
            Some(g?)
        } else {
            None
        };

        self.expect(T::FatArrow)?;

        let body = if self.at(T::LBrace) {
            MatchBody::Block(self.parse_block()?)
        } else {
            MatchBody::Expr(self.parse_expr()?)
        };

        // A trailing comma is optional; a newline ends the arm either way.
        self.eat(T::Comma);
        let span = start.to(self.prev_span());
        Some(MatchArm { pattern, guard, body, span })
    }

    fn parse_pattern(&mut self) -> Option<Pattern> {
        let start = self.span();
        let first = self.parse_single_pattern()?;
        if !self.at(T::Pipe) {
            return Some(first);
        }
        let mut alts = vec![first];
        while self.eat(T::Pipe) {
            alts.push(self.parse_single_pattern()?);
        }
        Some(Pattern::Or { alts, span: start.to(self.prev_span()) })
    }

    fn parse_single_pattern(&mut self) -> Option<Pattern> {
        let start = self.span();
        match self.peek() {
            T::Underscore => Some(Pattern::Wildcard(self.bump().span)),
            T::Nil => Some(Pattern::Nil(self.bump().span)),

            T::Int | T::Float | T::Str | T::Char | T::True | T::False | T::Minus => {
                let lit = self.parse_pattern_literal()?;
                if self.at(T::DotDot) || self.at(T::DotDotEq) {
                    let inclusive = self.at(T::DotDotEq);
                    self.bump();
                    let end = self.parse_pattern_literal()?;
                    return Some(Pattern::Range {
                        start: lit,
                        end,
                        inclusive,
                        span: start.to(self.prev_span()),
                    });
                }
                Some(Pattern::Literal(lit))
            }

            T::LParen => {
                self.bump();
                let mut elems = Vec::new();
                self.skip_newlines();
                while !self.at(T::RParen) && !self.at_end() {
                    elems.push(self.parse_pattern()?);
                    self.skip_newlines();
                    if !self.eat(T::Comma) {
                        break;
                    }
                    self.skip_newlines();
                }
                let end = self.expect(T::RParen)?;
                Some(Pattern::Tuple { elems, span: start.to(end) })
            }

            T::Ident => {
                // A bare name binds. A name followed by `(`, `{`, or `.` names
                // a variant or a struct; resolution decides which.
                let is_path = matches!(self.peek_at(1), T::LParen | T::LBrace | T::Dot);
                if !is_path {
                    return Some(Pattern::Binding(self.ident()?));
                }
                let path = self.parse_type_path()?;

                if self.at(T::LParen) {
                    self.bump();
                    self.skip_newlines();
                    let named = self.at(T::Ident) && self.peek_at(1) == T::Colon;
                    let args = if named {
                        let mut fields = Vec::new();
                        while !self.at(T::RParen) && !self.at_end() {
                            let name = self.ident()?;
                            self.expect(T::Colon)?;
                            let p = self.parse_pattern()?;
                            fields.push((name, p));
                            self.skip_newlines();
                            if !self.eat(T::Comma) {
                                break;
                            }
                            self.skip_newlines();
                        }
                        PatternArgs::Named(fields)
                    } else {
                        let mut pats = Vec::new();
                        while !self.at(T::RParen) && !self.at_end() {
                            pats.push(self.parse_pattern()?);
                            self.skip_newlines();
                            if !self.eat(T::Comma) {
                                break;
                            }
                            self.skip_newlines();
                        }
                        PatternArgs::Positional(pats)
                    };
                    let end = self.expect(T::RParen)?;
                    return Some(Pattern::Variant { path, args, span: start.to(end) });
                }

                if self.at(T::LBrace) {
                    self.bump();
                    let mut fields = Vec::new();
                    let mut rest = false;
                    self.skip_newlines();
                    while !self.at(T::RBrace) && !self.at_end() {
                        if self.eat(T::DotDot) {
                            rest = true;
                            self.skip_newlines();
                            break;
                        }
                        let f_start = self.span();
                        let name = self.ident()?;
                        // `Point{ x }` is shorthand that binds `x`.
                        let pattern = if self.eat(T::Colon) {
                            Some(self.parse_pattern()?)
                        } else {
                            None
                        };
                        fields.push(FieldPattern {
                            name,
                            pattern,
                            span: f_start.to(self.prev_span()),
                        });
                        self.skip_newlines();
                        if !self.eat(T::Comma) {
                            break;
                        }
                        self.skip_newlines();
                    }
                    let end = self.expect(T::RBrace)?;
                    return Some(Pattern::Struct { path, fields, rest, span: start.to(end) });
                }

                // A dotted path with no payload, such as `Status.Active`.
                Some(Pattern::Variant {
                    span: path.span,
                    path,
                    args: PatternArgs::Positional(Vec::new()),
                })
            }

            _ => {
                self.error_expected("a pattern");
                None
            }
        }
    }

    /// A literal usable in a pattern, including a negative number.
    fn parse_pattern_literal(&mut self) -> Option<Expr> {
        let start = self.span();
        if self.eat(T::Minus) {
            let inner = self.parse_primary()?;
            let span = start.to(inner.span());
            return Some(Expr::Unary {
                op: UnaryOp::Neg,
                operand: Box::new(inner),
                span,
            });
        }
        self.parse_primary()
    }

    fn parse_expr_or_assign(&mut self, start: Span) -> Option<Stmt> {
        let lhs = self.parse_expr()?;

        let op = match self.peek() {
            T::Eq => AssignOp::Assign,
            T::PlusEq => AssignOp::Add,
            T::MinusEq => AssignOp::Sub,
            T::StarEq => AssignOp::Mul,
            T::SlashEq => AssignOp::Div,
            T::PercentEq => AssignOp::Rem,
            _ => {
                let span = lhs.span();
                self.expect_terminator();
                let _ = span;
                return Some(Stmt::Expr(lhs));
            }
        };

        let op_span = self.bump().span;
        if !lhs.is_place() {
            self.diags.push(
                Diagnostic::error(codes::E0114, "cannot assign to this expression")
                    .with_primary(lhs.span(), format!("{} is not a place", lhs.describe()))
                    .with_secondary(op_span, "assignment target must be a name, field, or index"),
            );
        }
        let value = self.parse_expr()?;
        let span = start.to(value.span());
        self.expect_terminator();
        Some(Stmt::Assign(AssignStmt { target: lhs, op, value, span }))
    }

    // ---- expressions ------------------------------------------------------

    fn parse_expr(&mut self) -> Option<Expr> {
        self.parse_expr_bp(0)
    }

    /// Pratt loop. `min_bp` is the binding power the caller has already
    /// committed to; an operator binds here only if its left power exceeds it.
    fn parse_expr_bp(&mut self, min_bp: u8) -> Option<Expr> {
        let mut lhs = self.parse_prefix()?;

        #[allow(clippy::while_let_loop)]
        loop {
            let Some(op) = InfixOp::from_token(self.peek()) else {
                break;
            };
            let (lbp, rbp) = infix_binding_power(op);
            if lbp < min_bp {
                break;
            }

            match op {
                InfixOp::Cast => {
                    self.bump();
                    let ty = self.parse_type()?;
                    let span = lhs.span().to(ty.span());
                    lhs = Expr::Cast { expr: Box::new(lhs), ty, span };
                }
                InfixOp::Range { inclusive } => {
                    self.bump();
                    let rhs = self.parse_expr_bp(rbp)?;
                    let span = lhs.span().to(rhs.span());
                    lhs = Expr::Range {
                        start: Box::new(lhs),
                        end: Box::new(rhs),
                        inclusive,
                        span,
                    };
                }
                InfixOp::Binary(bop) => {
                    let op_span = self.span();
                    self.bump();
                    let rhs = self.parse_expr_bp(rbp)?;

                    // Comparison is non-associative: `a < b < c` is rejected
                    // rather than silently comparing a bool to an int.
                    if bop.is_comparison() {
                        if let Expr::Binary { op: inner, .. } = &lhs {
                            if inner.is_comparison() {
                                self.diags.push(
                                    Diagnostic::error(
                                        codes::E0100,
                                        "comparison operators cannot be chained",
                                    )
                                    .with_primary(op_span, "second comparison here")
                                    .with_secondary(lhs.span(), "first comparison here")
                                    .with_note(
                                        "write `a < b && b < c` to compare three values",
                                    ),
                                );
                            }
                        }
                    }

                    let span = lhs.span().to(rhs.span());
                    lhs = Expr::Binary {
                        op: bop,
                        lhs: Box::new(lhs),
                        rhs: Box::new(rhs),
                        span,
                    };
                }
            }
        }

        Some(lhs)
    }

    /// Prefix operators bind looser than postfix ones, so the recursive call
    /// consumes the postfix chain before the prefix operator wraps it:
    /// `-x.foo` is `-(x.foo)`, and `await f()` is `await (f())`.
    fn parse_prefix(&mut self) -> Option<Expr> {
        let start = self.span();
        match self.peek() {
            T::Minus => {
                self.bump();
                let operand = self.parse_prefix()?;
                let span = start.to(operand.span());
                Some(Expr::Unary { op: UnaryOp::Neg, operand: Box::new(operand), span })
            }
            T::Bang => {
                self.bump();
                let operand = self.parse_prefix()?;
                let span = start.to(operand.span());
                Some(Expr::Unary { op: UnaryOp::Not, operand: Box::new(operand), span })
            }
            T::Await => {
                self.bump();
                let operand = self.parse_prefix()?;
                let span = start.to(operand.span());
                Some(Expr::Await { expr: Box::new(operand), span })
            }
            _ => {
                let primary = self.parse_primary()?;
                self.parse_postfix(primary)
            }
        }
    }

    fn parse_postfix(&mut self, mut expr: Expr) -> Option<Expr> {
        loop {
            match self.peek() {
                T::Dot => {
                    self.bump();
                    // `pair.0` — a tuple's elements are positional, so the
                    // "name" after the dot is a number. It is kept as text
                    // from here on, because a field is a field.
                    let name = if self.at(T::Int) {
                        let span = self.span();
                        self.bump();
                        Ident::new(self.text(span), span)
                    } else {
                        self.ident()?
                    };
                    let span = expr.span().to(name.span);
                    // `.` always produces a field access. Whether `io.print` is
                    // really a module path rather than a field of a local is a
                    // resolution question, not a syntactic one, so the resolver
                    // decides and records its answer against this span.
                    expr = Expr::Field { base: Box::new(expr), name, span };
                }
                T::LParen => {
                    self.bump();
                    let mut args = Vec::new();
                    let mut arg_names = Vec::new();
                    self.skip_newlines();
                    while !self.at(T::RParen) && !self.at_end() {
                        // `Circle(radius: 2.0)` names its payload field. A
                        // plain `f(x)` does not.
                        let name = if self.at(T::Ident) && self.peek_at(1) == T::Colon {
                            let n = self.ident()?;
                            self.bump(); // `:`
                            Some(n)
                        } else {
                            None
                        };
                        arg_names.push(name);
                        args.push(self.in_brackets(|p| p.parse_expr())?);
                        self.skip_newlines();
                        if !self.eat(T::Comma) {
                            break;
                        }
                        self.skip_newlines();
                    }
                    let end = self.expect(T::RParen)?;
                    let span = expr.span().to(end);
                    expr = Expr::Call { callee: Box::new(expr), args, arg_names, span };
                }
                T::LBracket => {
                    self.bump();
                    let index = self.in_brackets(|p| p.parse_expr())?;
                    let end = self.expect(T::RBracket)?;
                    let span = expr.span().to(end);
                    expr = Expr::Index {
                        base: Box::new(expr),
                        index: Box::new(index),
                        span,
                    };
                }
                // `Point{ x: 1.0 }`. Suppressed inside an `if`/`for`/`match`
                // scrutinee, where `{` opens the body; the specification tells
                // the user to parenthesise in that position.
                T::LBrace if self.no_struct_literal == 0 => {
                    // `ui.Style{ … }` reaches here as a field access, because
                    // `a.b` is a resolution question rather than a syntactic
                    // one. Only a chain of plain names can be a type, so
                    // anything else stays a field access and the `{` is left
                    // for whatever follows.
                    let Some(path) = type_path_of(&expr) else {
                        return Some(expr);
                    };
                    expr = Expr::StructLit(self.parse_struct_literal(path)?);
                }
                _ => return Some(expr),
            }
        }
    }

    fn parse_primary(&mut self) -> Option<Expr> {
        let span = self.span();
        match self.peek() {
            T::Int => {
                self.bump();
                Some(Expr::Int(span))
            }
            T::Float => {
                self.bump();
                Some(Expr::Float(span))
            }
            T::Str => {
                self.bump();
                match self.split_interpolation(span) {
                    Some(parts) => Some(Expr::Interpolated { parts, span }),
                    None => Some(Expr::Str(span)),
                }
            }
            T::Char => {
                self.bump();
                Some(Expr::Char(span))
            }
            T::True => {
                self.bump();
                Some(Expr::Bool { value: true, span })
            }
            T::False => {
                self.bump();
                Some(Expr::Bool { value: false, span })
            }
            T::Nil => {
                self.bump();
                Some(Expr::Nil(span))
            }
            T::SelfKw => {
                self.bump();
                Some(Expr::SelfExpr(span))
            }
            T::Ident => {
                let name = self.ident()?;
                Some(Expr::Path(Path { span: name.span, segments: vec![name] }))
            }
            T::If => {
                let if_stmt = self.parse_if()?;
                let Some(else_) = if_stmt.else_ else {
                    self.diags.push(
                        Diagnostic::error(codes::E0100, "`if` used as a value needs an `else`")
                            .with_primary(if_stmt.span, "this `if` produces no value on one path")
                            .with_note("every branch of a value `if` must yield a value"),
                    );
                    return Some(Expr::Error(if_stmt.span));
                };
                Some(Expr::If {
                    cond: Box::new(if_stmt.cond),
                    then: if_stmt.then,
                    else_,
                    span: if_stmt.span,
                })
            }
            T::LParen => {
                self.bump();
                self.skip_newlines();
                if self.at(T::RParen) {
                    let end = self.bump().span;
                    return Some(Expr::Tuple { elems: Vec::new(), span: span.to(end) });
                }
                let first = self.in_brackets(|p| p.parse_expr())?;
                self.skip_newlines();
                if self.eat(T::Comma) {
                    let mut elems = vec![first];
                    self.skip_newlines();
                    while !self.at(T::RParen) && !self.at_end() {
                        elems.push(self.in_brackets(|p| p.parse_expr())?);
                        self.skip_newlines();
                        if !self.eat(T::Comma) {
                            break;
                        }
                        self.skip_newlines();
                    }
                    let end = self.expect(T::RParen)?;
                    return Some(Expr::Tuple { elems, span: span.to(end) });
                }
                let end = self.expect(T::RParen)?;
                Some(Expr::Paren { inner: Box::new(first), span: span.to(end) })
            }
            T::LBracket => {
                self.bump();
                let mut elems = Vec::new();
                self.skip_newlines();
                while !self.at(T::RBracket) && !self.at_end() {
                    elems.push(self.in_brackets(|p| p.parse_expr())?);
                    self.skip_newlines();
                    if !self.eat(T::Comma) {
                        break;
                    }
                    self.skip_newlines();
                }
                let end = self.expect(T::RBracket)?;
                Some(Expr::Slice { elems, span: span.to(end) })
            }
            T::Match => Some(Expr::Match(self.parse_match()?)),
            T::LBrace if self.no_struct_literal == 0 => self.parse_map_literal(),
            T::Pipe | T::PipePipe => Some(self.parse_closure()?),
            _ => {
                self.error_expected("an expression");
                None
            }
        }
    }

    /// Split a string literal at its `\(expr)` holes, parsing each one as an
    /// ordinary expression. Returns `None` when there are no holes, which is
    /// the common case and keeps a plain literal a plain literal.
    ///
    /// The holes are parsed here rather than left as text for a later phase so
    /// that a syntax error inside one is reported by the parser, with a span
    /// pointing into the string — which is why the sub-lexer is given the whole
    /// file and a range rather than a copied fragment.
    fn split_interpolation(&mut self, span: Span) -> Option<Vec<StrPart>> {
        let start = span.start as usize;
        let raw = &self.src[start..span.end as usize];
        // Skip the opening delimiter; the closing one ends the scan naturally,
        // because a `\` cannot be the last byte of a well-formed literal.
        let open = if raw.starts_with("\"\"\"") { 3 } else { 1 };

        let bytes = raw.as_bytes();
        let mut parts: Vec<StrPart> = Vec::new();
        let mut run = open;
        let mut i = open;
        while i + 1 < bytes.len() {
            if bytes[i] != b'\\' {
                i += 1;
                continue;
            }
            if bytes[i + 1] != b'(' {
                // Some other escape. Step over both bytes so `\\\\(` is a
                // literal backslash followed by a paren, not a hole.
                i += 2;
                continue;
            }
            if run < i {
                parts.push(StrPart::Text(Span::new(
                    self.file,
                    (start + run) as u32,
                    (start + i) as u32,
                )));
            }
            let open_paren = i + 1;
            let Some(close) = matching_paren(bytes, open_paren) else {
                self.diags.push(
                    kite_diag::Diagnostic::error(
                        kite_diag::codes::E0100,
                        "unterminated interpolation",
                    )
                    .with_primary(
                        Span::new(self.file, (start + i) as u32, (start + open_paren + 1) as u32),
                        "this `\\(` is never closed",
                    ),
                );
                return Some(parts);
            };
            parts.push(StrPart::Hole(self.parse_hole(
                start + open_paren + 1,
                start + close,
            )));
            i = close + 1;
            run = i;
        }
        if parts.is_empty() {
            return None;
        }
        let text_end = bytes.len().saturating_sub(open);
        if run < text_end {
            parts.push(StrPart::Text(Span::new(
                self.file,
                (start + run) as u32,
                (start + text_end) as u32,
            )));
        }
        Some(parts)
    }

    /// Parse one hole's expression, over a byte range of the file.
    fn parse_hole(&mut self, start: usize, end: usize) -> Expr {
        let tokens = kite_lexer::tokenize_range(self.file, self.src, start, end, self.diags);
        let mut sub = Parser {
            file: self.file,
            src: self.src,
            tokens: &tokens,
            pos: 0,
            diags: self.diags,
            panicking: false,
            no_struct_literal: 0,
            in_hole: true,
            split_gt: false,
        };
        let span = Span::new(self.file, start as u32, end as u32);
        match sub.parse_expr() {
            Some(e) => e,
            None => Expr::Error(span),
        }
    }

    /// The `{ .. }` of a struct literal. `path` has already been consumed.
    fn parse_struct_literal(&mut self, path: TypePath) -> Option<StructLit> {
        let start = path.span;
        self.bump(); // `{`
        self.skip_newlines();

        // `Point{ ..p, y: 5.0 }` produces a new value; it never mutates `p`.
        let base = if self.eat(T::DotDot) {
            let b = self.parse_expr()?;
            self.skip_newlines();
            self.eat(T::Comma);
            self.skip_newlines();
            Some(Box::new(b))
        } else {
            None
        };

        let mut fields = Vec::new();
        while !self.at(T::RBrace) && !self.at_end() {
            let f_start = self.span();
            let name = self.ident()?;
            // `Point{ x }` is shorthand for `Point{ x: x }`.
            let value = if self.eat(T::Colon) {
                self.parse_expr()?
            } else {
                Expr::Path(Path { span: name.span, segments: vec![name.clone()] })
            };
            fields.push(FieldInit { name, value, span: f_start.to(self.prev_span()) });
            self.skip_newlines();
            if !self.eat(T::Comma) {
                break;
            }
            self.skip_newlines();
        }
        let end = self.expect(T::RBrace)?;
        Some(StructLit { path, base, fields, span: start.to(end) })
    }

    /// `{"a": 1, "b": 2}`. Kite has no block expressions, so a `{` in
    /// expression position is unambiguously a map.
    fn parse_map_literal(&mut self) -> Option<Expr> {
        let start = self.span();
        self.bump(); // `{`
        let mut entries = Vec::new();
        self.skip_newlines();
        while !self.at(T::RBrace) && !self.at_end() {
            let key = self.parse_expr()?;
            self.expect(T::Colon)?;
            let value = self.parse_expr()?;
            entries.push(MapEntry { key, value });
            self.skip_newlines();
            if !self.eat(T::Comma) {
                break;
            }
            self.skip_newlines();
        }
        let end = self.expect(T::RBrace)?;
        Some(Expr::Map { entries, span: start.to(end) })
    }

    fn parse_closure(&mut self) -> Option<Expr> {
        let start = self.span();
        let mut params = Vec::new();

        if self.eat(T::PipePipe) {
            // `||` with no parameters.
        } else {
            self.bump(); // `|`
            while !self.at(T::Pipe) && !self.at_end() {
                let name = self.ident()?;
                let ty = if self.eat(T::Colon) {
                    Some(self.parse_type()?)
                } else {
                    None
                };
                params.push(ClosureParam { name, ty });
                if !self.eat(T::Comma) {
                    break;
                }
            }
            self.expect(T::Pipe)?;
        }

        // A block body has nothing to infer a return type from unless the
        // context supplies one, so `-> T` is available for the cases it does
        // not — `let f = |x: int| -> str { … }`.
        let ret = if self.eat(T::Arrow) {
            Some(Box::new(self.parse_type()?))
        } else {
            None
        };

        let body = if self.at(T::LBrace) {
            ClosureBody::Block(self.parse_block()?)
        } else {
            ClosureBody::Expr(self.parse_expr()?)
        };
        let span = start.to(self.prev_span());
        Some(Expr::Closure { params, ret, body: Box::new(body), span })
    }
}

#[cfg(test)]
mod tests;
