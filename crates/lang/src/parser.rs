//! Recursive-descent parser producing the AST.

use std::collections::HashSet;

use crate::ast::*;
use crate::diag::{CompileError, Span};
use crate::lexer::{Kw, Tok, Token};

pub fn parse(tokens: &[Token]) -> Result<Program, CompileError> {
    let (structs, enums, aliases) = scan_type_names(tokens);
    let mut parser = Parser {
        tokens,
        pos: 0,
        structs,
        enums,
        aliases,
    };
    let mut items = Vec::new();

    while !parser.at(Tok::Eof) {
        items.push(parser.parse_item()?);
    }

    Ok(Program { items })
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
    structs: HashSet<String>,
    enums: HashSet<String>,
    aliases: HashSet<String>,
}

/// Pre-scan declared type names so `Name { .. }` aggregates, `Name.Variant`
/// fields, and type positions can be disambiguated while parsing.
fn scan_type_names(tokens: &[Token]) -> (HashSet<String>, HashSet<String>, HashSet<String>) {
    let mut structs = HashSet::new();
    let mut enums = HashSet::new();
    let mut aliases = HashSet::new();
    for (i, tok) in tokens.iter().enumerate() {
        let Some(next) = tokens.get(i + 1) else {
            continue;
        };
        match &tok.kind {
            Tok::Kw(Kw::Struct) => {
                if let Tok::Ident(name) = &next.kind {
                    structs.insert(name.clone());
                }
            }
            Tok::Kw(Kw::Enum) => {
                if let Tok::Ident(name) = &next.kind {
                    enums.insert(name.clone());
                }
            }
            Tok::Kw(Kw::Type) => {
                if let Tok::Ident(name) = &next.kind {
                    aliases.insert(name.clone());
                }
            }
            _ => {}
        }
    }
    (structs, enums, aliases)
}

impl<'a> Parser<'a> {
    fn peek(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn peek_kind(&self) -> &Tok {
        &self.peek().kind
    }

    fn at(&self, kind: Tok) -> bool {
        *self.peek_kind() == kind
    }

    fn nth_kind(&self, offset: usize) -> Option<&Tok> {
        self.tokens.get(self.pos + offset).map(|t| &t.kind)
    }

    fn advance(&mut self) -> &Token {
        let token = &self.tokens[self.pos];
        if !self.at(Tok::Eof) {
            self.pos += 1;
        }
        token
    }

    fn eat(&mut self, kind: Tok) -> bool {
        if self.at(kind) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, kind: Tok) -> Result<&Token, CompileError> {
        let expected = kind.token();
        if self.at(kind) {
            Ok(self.advance())
        } else {
            Err(self.err_here(format!(
                "expected `{}`, found `{}`",
                expected,
                self.peek().text()
            )))
        }
    }

    fn expect_ident(&mut self) -> Result<String, CompileError> {
        if let Tok::Ident(name) = self.peek_kind() {
            let name = name.clone();
            self.advance();
            return Ok(name);
        }
        Err(self.err_here(format!(
            "expected an identifier, found `{}`",
            self.peek().text()
        )))
    }

    fn expect_int(&mut self) -> Result<i64, CompileError> {
        if let Tok::Int(value) = self.peek_kind() {
            let value = *value;
            self.advance();
            return Ok(value);
        }
        Err(self.err_here(format!(
            "expected an integer, found `{}`",
            self.peek().text()
        )))
    }

    fn err_here(&self, message: impl Into<String>) -> CompileError {
        let token = self.peek();
        CompileError::new(message, token.line, token.col)
    }

    fn previous(&self) -> &Token {
        &self.tokens[self.pos - 1]
    }

    /// Whether the current token begins a declaration: a type, `void`, or
    /// prefix pointer stars in front of a type.
    fn at_type_start(&self) -> bool {
        match self.peek_kind() {
            Tok::Kw(Kw::Void) => true,
            Tok::Ident(name) => self.is_type_name(name),
            Tok::Star => {
                let mut i = 0;
                while matches!(self.nth_kind(i), Some(Tok::Star)) {
                    i += 1;
                }
                match self.nth_kind(i) {
                    Some(Tok::Kw(Kw::Void)) => true,
                    Some(Tok::Ident(name)) => self.is_type_name(name),
                    _ => false,
                }
            }
            _ => false,
        }
    }

    fn is_type_name(&self, name: &str) -> bool {
        primitive_type(name).is_some()
            || self.structs.contains(name)
            || self.enums.contains(name)
            || self.aliases.contains(name)
    }

    fn parse_item(&mut self) -> Result<Item, CompileError> {
        match self.peek_kind().clone() {
            Tok::Kw(Kw::Struct) => self.parse_struct().map(Item::Struct),
            Tok::Kw(Kw::Enum) => self.parse_enum().map(Item::Enum),
            Tok::Kw(Kw::Type) => self.parse_type_alias().map(Item::TypeAlias),
            Tok::Kw(Kw::Entry) => self.parse_entry().map(Item::Entry),
            Tok::Kw(Kw::Static) => {
                self.advance();
                self.parse_global(true).map(Item::Global)
            }
            _ if self.at_type_start() => self.parse_func_or_global(),
            _ => Err(self.err_here(format!(
                "expected `struct`, `enum`, `type`, `entry`, or a type, found `{}`",
                self.peek().text()
            ))),
        }
    }

    fn parse_struct(&mut self) -> Result<StructDecl, CompileError> {
        let start = self.expect(Tok::Kw(Kw::Struct))?.span;
        let name = self.expect_ident()?;
        let mut packed = false;
        if let Tok::Ident(word) = self.peek_kind()
            && word == "layout"
        {
            self.advance();
            self.expect(Tok::LParen)?;
            let flag = self.expect_ident()?;
            if flag != "packed" {
                return Err(self.err_here(format!(
                    "unknown layout option `{flag}` (expected `packed`)"
                )));
            }
            self.expect(Tok::RParen)?;
            packed = true;
        }
        self.expect(Tok::LBrace)?;
        let mut fields = Vec::new();
        while !self.at(Tok::RBrace) {
            let fs = self.peek().span;
            let ty = self.parse_type()?;
            if matches!(ty, Type::Void) {
                return Err(self.err_here("`void` is not a valid field type"));
            }
            let fname = self.expect_ident()?;
            let ty = self.parse_array_suffix(ty)?;
            let fe = self.previous().span;
            fields.push(Field {
                name: fname,
                ty,
                span: fs.to(fe),
            });
            self.expect(Tok::Semi)?;
        }
        let end = self.expect(Tok::RBrace)?.span;
        self.expect(Tok::Semi)?;
        Ok(StructDecl {
            name,
            packed,
            fields,
            span: start.to(end),
        })
    }

    fn parse_enum(&mut self) -> Result<EnumDecl, CompileError> {
        let start = self.expect(Tok::Kw(Kw::Enum))?.span;
        let name = self.expect_ident()?;
        let underlying = if self.eat(Tok::Colon) {
            let ty = self.parse_type_base()?;
            match primitive_type_int(&ty) {
                Some(ty) => ty,
                None => {
                    return Err(self.err_here(
                        "enum underlying type must be a fixed-width integer or word/sword",
                    ));
                }
            }
        } else {
            Type::U32
        };
        self.expect(Tok::LBrace)?;
        let mut variants = Vec::new();
        while !self.at(Tok::RBrace) {
            let vs = self.peek().span;
            let vname = self.expect_ident()?;
            let value = if self.eat(Tok::Assign) {
                Some(self.expect_int()?)
            } else {
                None
            };
            let ve = self.previous().span;
            variants.push(Variant {
                name: vname,
                value,
                span: vs.to(ve),
            });
            if !self.eat(Tok::Comma) {
                break;
            }
        }
        let end = self.expect(Tok::RBrace)?.span;
        self.expect(Tok::Semi)?;
        Ok(EnumDecl {
            name,
            underlying,
            variants,
            span: start.to(end),
        })
    }

    fn parse_type_alias(&mut self) -> Result<TypeAlias, CompileError> {
        let start = self.expect(Tok::Kw(Kw::Type))?.span;
        let name = self.expect_ident()?;
        self.expect(Tok::Assign)?;
        let ty = self.parse_type()?;
        let end = self.expect(Tok::Semi)?.span;
        Ok(TypeAlias {
            name,
            ty,
            span: start.to(end),
        })
    }

    fn parse_entry(&mut self) -> Result<EntryDecl, CompileError> {
        let start = self.expect(Tok::Kw(Kw::Entry))?.span;
        let name = self.expect_ident()?;
        let end = self.expect(Tok::Semi)?.span;
        Ok(EntryDecl {
            name,
            span: start.to(end),
        })
    }

    /// Parse a non-void function or a top-level global. Dispatch on a `(` or
    /// `[` lookahead after the declarator name.
    fn parse_func_or_global(&mut self) -> Result<Item, CompileError> {
        let start = self.peek().span;
        let ty = self.parse_type()?;
        let name = self.expect_ident()?;
        if self.at(Tok::LParen) {
            if self.array_depth(&ty) > 0 {
                return Err(self.err_here("functions cannot return arrays in this position"));
            }
            let params = self.parse_params()?;
            let body = self.parse_block()?;
            let span = start.to(body.span);
            return Ok(Item::Func(Func {
                name,
                params,
                ret_ty: ty,
                body,
                span,
            }));
        }
        if matches!(ty, Type::Void) {
            return Err(self.err_here("`void` is not a valid storage type"));
        }
        self.parse_global_tail(start, name, ty, false)
            .map(Item::Global)
    }

    /// Parse the remainder of a global declaration after `Type name` (or after
    /// a consumed `static` keyword).
    fn parse_global(&mut self, explicit_static: bool) -> Result<Global, CompileError> {
        let start = self.previous().span;
        let ty = self.parse_type()?;
        if matches!(ty, Type::Void) {
            return Err(self.err_here("`void` is not a valid storage type"));
        }
        let name = self.expect_ident()?;
        self.parse_global_tail(start, name, ty, explicit_static)
    }

    fn parse_global_tail(
        &mut self,
        start: Span,
        name: String,
        ty: Type,
        explicit_static: bool,
    ) -> Result<Global, CompileError> {
        let ty = self.parse_array_suffix(ty)?;
        let value = if self.eat(Tok::Assign) {
            Some(self.parse_expr()?)
        } else {
            None
        };
        let end = self.expect(Tok::Semi)?.span;
        Ok(Global {
            name,
            ty,
            value,
            explicit_static,
            span: start.to(end),
        })
    }

    fn array_depth(&self, ty: &Type) -> usize {
        match ty {
            Type::Array { elem, .. } => 1 + self.array_depth(elem),
            _ => 0,
        }
    }

    fn parse_params(&mut self) -> Result<Vec<Param>, CompileError> {
        self.expect(Tok::LParen)?;
        let mut params = Vec::new();
        loop {
            if self.at(Tok::RParen) {
                break;
            }
            params.push(self.parse_param()?);
            if !self.eat(Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RParen)?;
        Ok(params)
    }

    fn parse_param(&mut self) -> Result<Param, CompileError> {
        let ty = self.parse_type()?;
        if matches!(ty, Type::Void) {
            return Err(self.err_here("`void` is not a valid parameter type"));
        }
        let name = self.expect_ident()?;
        if self.at(Tok::LBracket) {
            return Err(self.err_here("array parameters are not supported; use a pointer `*T`"));
        }
        Ok(Param { name, ty })
    }

    /// A type in storage position: prefix pointer stars, a base type, and any
    /// trailing array suffixes are parsed by the caller via
    /// [`Self::parse_array_suffix`].
    fn parse_type(&mut self) -> Result<Type, CompileError> {
        let mut stars = 0usize;
        while self.at(Tok::Star) {
            self.advance();
            stars += 1;
        }
        let mut ty = self.parse_type_base()?;
        for _ in 0..stars {
            ty = Type::Ptr(Box::new(ty));
        }
        Ok(ty)
    }

    /// Array `[N]` / multi-dim `[N][M]` suffixes.
    fn parse_array_suffix(&mut self, mut ty: Type) -> Result<Type, CompileError> {
        while self.at(Tok::LBracket) {
            self.advance();
            let len = self.expect_array_len()?;
            self.expect(Tok::RBracket)?;
            ty = Type::Array {
                elem: Box::new(ty),
                len,
            };
        }
        Ok(ty)
    }

    fn expect_array_len(&mut self) -> Result<usize, CompileError> {
        let len = self.expect_int()?;
        if len < 0 {
            return Err(self.err_here("array length must be non-negative"));
        }
        usize::try_from(len).map_err(|_| self.err_here("array length does not fit `usize`"))
    }

    fn parse_type_base(&mut self) -> Result<Type, CompileError> {
        match self.peek_kind().clone() {
            Tok::Kw(Kw::Void) => {
                self.advance();
                Ok(Type::Void)
            }
            Tok::Ident(name) => {
                if let Some(ty) = primitive_type(&name) {
                    self.advance();
                    return Ok(ty);
                }
                if self.structs.contains(&name) {
                    self.advance();
                    return Ok(Type::Struct(name));
                }
                if self.enums.contains(&name) {
                    self.advance();
                    return Ok(Type::Enum(name));
                }
                if self.aliases.contains(&name) {
                    self.advance();
                    return Ok(Type::Alias(name));
                }
                Err(self.err_here(format!(
                    "unknown type `{name}` (expected bool, void, i8..i64, u8..u64, word, sword, `*T`, or `T[N]`)"
                )))
            }
            _ => Err(self.err_here(format!("expected a type, found `{}`", self.peek().text()))),
        }
    }

    //statements

    fn parse_block(&mut self) -> Result<Block, CompileError> {
        let start = self.expect(Tok::LBrace)?.span;
        let mut stmts = Vec::new();

        while !self.at(Tok::RBrace) {
            if self.at(Tok::Eof) {
                return Err(self.err_here("expected `}` to close block"));
            }
            stmts.push(self.parse_stmt()?);
        }

        let end = self.advance().span;
        Ok(Block {
            stmts,
            span: start.to(end),
        })
    }

    fn parse_stmt(&mut self) -> Result<Stmt, CompileError> {
        match self.peek_kind().clone() {
            Tok::Kw(Kw::Static) => {
                Err(self
                    .err_here("`static` locals are not supported; use a top-level `static` object"))
            }
            Tok::Kw(Kw::Stack) => {
                self.advance();
                self.parse_decl_stmt(Storage::Stack)
            }
            _ if self.at_type_start() => self.parse_decl_stmt(Storage::Auto),
            Tok::Kw(Kw::If) => self.parse_if(),
            Tok::Kw(Kw::While) => self.parse_while(),
            Tok::Kw(Kw::For) => self.parse_for(),
            Tok::Kw(Kw::Break) => {
                let span = self.advance().span;
                self.expect(Tok::Semi)?;
                Ok(Stmt::Break(span))
            }
            Tok::Kw(Kw::Continue) => {
                let span = self.advance().span;
                self.expect(Tok::Semi)?;
                Ok(Stmt::Continue(span))
            }
            Tok::Kw(Kw::Return) => self.parse_return(),
            _ => self.parse_expr_stmt(),
        }
    }

    fn parse_decl_stmt(&mut self, storage: Storage) -> Result<Stmt, CompileError> {
        let start = self.peek().span;
        let ty = self.parse_type()?;
        if matches!(ty, Type::Void) {
            return Err(self.err_here("`void` is not a valid storage type"));
        }
        let name = self.expect_ident()?;
        let ty = self.parse_array_suffix(ty)?;
        let value = if self.eat(Tok::Assign) {
            Some(self.parse_expr()?)
        } else {
            None
        };
        let end = self.expect(Tok::Semi)?.span;
        Ok(Stmt::Decl {
            name,
            ty,
            value,
            storage,
            span: start.to(end),
        })
    }

    fn parse_paren_cond(&mut self) -> Result<Expr, CompileError> {
        self.expect(Tok::LParen)?;
        let cond = self.parse_expr()?;
        self.expect(Tok::RParen)?;
        Ok(cond)
    }

    fn parse_if(&mut self) -> Result<Stmt, CompileError> {
        let start = self.expect(Tok::Kw(Kw::If))?.span;
        let cond = self.parse_paren_cond()?;
        let then_branch = self.parse_block()?;
        let else_branch = if self.eat(Tok::Kw(Kw::Else)) {
            if self.at(Tok::Kw(Kw::If)) {
                let nested = self.parse_if()?;
                let span = nested.span();
                Some(Block {
                    stmts: vec![nested],
                    span,
                })
            } else {
                Some(self.parse_block()?)
            }
        } else {
            None
        };

        let end = else_branch
            .as_ref()
            .map(|b| b.span)
            .unwrap_or(then_branch.span);
        Ok(Stmt::If {
            cond,
            then_branch,
            else_branch,
            span: start.to(end),
        })
    }

    fn parse_while(&mut self) -> Result<Stmt, CompileError> {
        let start = self.expect(Tok::Kw(Kw::While))?.span;
        let cond = self.parse_paren_cond()?;
        let body = self.parse_block()?;
        let span = start.to(body.span);
        Ok(Stmt::While { cond, body, span })
    }

    fn parse_for(&mut self) -> Result<Stmt, CompileError> {
        let start = self.expect(Tok::Kw(Kw::For))?.span;
        self.expect(Tok::LParen)?;
        let init = if self.at(Tok::Semi) {
            self.advance();
            None
        } else if self.at_type_start() {
            let decl = self.parse_decl_stmt(Storage::Auto)?;
            Some(Box::new(decl))
        } else {
            let expr = self.parse_expr()?;
            self.expect(Tok::Semi)?;
            Some(Box::new(Stmt::Expr(expr)))
        };
        let cond = if self.at(Tok::Semi) {
            None
        } else {
            Some(self.parse_expr()?)
        };
        self.expect(Tok::Semi)?;
        let step = if self.at(Tok::RParen) {
            None
        } else {
            Some(Box::new(self.parse_for_step()?))
        };
        self.expect(Tok::RParen)?;
        let body = self.parse_block()?;
        let span = start.to(body.span);
        Ok(Stmt::For {
            init,
            cond,
            step,
            body,
            span,
        })
    }

    /// A `for` step: an expression or an assignment, without a semicolon.
    fn parse_for_step(&mut self) -> Result<Stmt, CompileError> {
        let expr = self.parse_expr()?;
        if self.at(Tok::Assign) {
            if !is_lvalue(&expr) {
                return Err(self.err_here("invalid assignment target"));
            }
            self.advance();
            let value = self.parse_expr()?;
            let span = expr.span().to(value.span());
            Ok(Stmt::Assign {
                target: expr,
                value,
                span,
            })
        } else {
            Ok(Stmt::Expr(expr))
        }
    }

    fn parse_return(&mut self) -> Result<Stmt, CompileError> {
        let start = self.expect(Tok::Kw(Kw::Return))?.span;
        let value = if self.at(Tok::Semi) {
            None
        } else {
            Some(self.parse_expr()?)
        };
        let end = self.expect(Tok::Semi)?.span;
        Ok(Stmt::Return {
            value,
            span: start.to(end),
        })
    }

    fn parse_expr_stmt(&mut self) -> Result<Stmt, CompileError> {
        let expr = self.parse_expr()?;
        if self.at(Tok::Assign) {
            if !is_lvalue(&expr) {
                return Err(self.err_here("invalid assignment target"));
            }
            self.advance();
            let value = self.parse_expr()?;
            let span = expr.span().to(value.span());
            self.expect(Tok::Semi)?;
            Ok(Stmt::Assign {
                target: expr,
                value,
                span,
            })
        } else {
            self.expect(Tok::Semi)?;
            Ok(Stmt::Expr(expr))
        }
    }

    // expr

    fn parse_expr(&mut self) -> Result<Expr, CompileError> {
        self.parse_binary(1)
    }

    fn parse_binary(&mut self, min_prec: u8) -> Result<Expr, CompileError> {
        let mut lhs = self.parse_unary()?;

        while let Some((prec, op)) = self.binop() {
            if prec < min_prec {
                break;
            }
            self.advance();
            let rhs = self.parse_binary(prec + 1)?;
            let span = lhs.span().to(rhs.span());
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }

        Ok(lhs)
    }

    fn binop(&self) -> Option<(u8, BinOp)> {
        Some(match self.peek_kind() {
            Tok::OrOr => (1, BinOp::OrOr),
            Tok::AndAnd => (2, BinOp::AndAnd),
            Tok::Pipe => (3, BinOp::Or),
            Tok::Caret => (4, BinOp::Xor),
            Tok::Amp => (5, BinOp::And),
            Tok::Eq => (6, BinOp::Eq),
            Tok::Ne => (6, BinOp::Ne),
            Tok::Lt => (7, BinOp::Lt),
            Tok::Le => (7, BinOp::Le),
            Tok::Gt => (7, BinOp::Gt),
            Tok::Ge => (7, BinOp::Ge),
            Tok::Shl => (8, BinOp::Shl),
            Tok::Shr => (8, BinOp::Shr),
            Tok::Plus => (9, BinOp::Add),
            Tok::Minus => (9, BinOp::Sub),
            Tok::Star => (10, BinOp::Mul),
            Tok::Slash => (10, BinOp::Div),
            Tok::Percent => (10, BinOp::Mod),
            _ => return None,
        })
    }

    fn parse_unary(&mut self) -> Result<Expr, CompileError> {
        let op = match self.peek_kind() {
            Tok::Minus => Some(UnOp::Neg),
            Tok::Bang => Some(UnOp::Not),
            Tok::Tilde => Some(UnOp::BitNot),
            Tok::Amp => Some(UnOp::AddrOf),
            Tok::Star => Some(UnOp::Deref),
            _ => None,
        };

        if let Some(op) = op {
            let start = self.advance().span;
            let expr = self.parse_unary()?;
            let span = start.to(expr.span());
            return Ok(Expr::Unary {
                op,
                expr: Box::new(expr),
                span,
            });
        }

        self.parse_postfix()
    }

    fn parse_postfix(&mut self) -> Result<Expr, CompileError> {
        let mut expr = self.parse_primary()?;

        loop {
            if self.at(Tok::LParen) {
                self.advance();
                let mut args = Vec::new();
                loop {
                    if self.at(Tok::RParen) {
                        break;
                    }
                    args.push(self.parse_expr()?);
                    if !self.eat(Tok::Comma) {
                        break;
                    }
                }
                let close = self.expect(Tok::RParen)?.span;
                let span = expr.span().to(close);
                expr = Expr::Call {
                    callee: Box::new(expr),
                    args,
                    span,
                };
            } else if self.at(Tok::LBracket) {
                self.advance();
                let index = self.parse_expr()?;
                let close = self.expect(Tok::RBracket)?.span;
                let span = expr.span().to(close);
                expr = Expr::Index {
                    base: Box::new(expr),
                    index: Box::new(index),
                    span,
                };
            } else if self.at(Tok::Dot) {
                self.advance();
                let name = self.expect_ident()?;
                let span = expr.span().to(self.previous().span);
                expr = Expr::Field {
                    base: Box::new(expr),
                    name,
                    span,
                };
            } else {
                break;
            }
        }

        Ok(expr)
    }

    fn parse_primary(&mut self) -> Result<Expr, CompileError> {
        let kind = self.peek_kind().clone();
        match kind {
            Tok::Int(value) => {
                let span = self.advance().span;
                Ok(Expr::Int(value, span))
            }
            Tok::Str(bytes) => {
                let span = self.advance().span;
                Ok(Expr::Str(bytes, span))
            }
            Tok::Kw(Kw::True) => {
                let span = self.advance().span;
                Ok(Expr::Bool(true, span))
            }
            Tok::Kw(Kw::False) => {
                let span = self.advance().span;
                Ok(Expr::Bool(false, span))
            }
            Tok::Kw(Kw::Null) => {
                let span = self.advance().span;
                Ok(Expr::Null(span))
            }
            Tok::Ident(name) => {
                if let Some(expr) = self.try_parse_intrinsic(&name)? {
                    return Ok(expr);
                }
                let span = self.advance().span;
                Ok(Expr::Ident(name, span))
            }
            Tok::LParen => {
                self.advance();
                let expr = self.parse_expr()?;
                self.expect(Tok::RParen)?;
                Ok(expr)
            }
            Tok::LBracket => {
                let start = self.advance().span;
                let mut elems = Vec::new();
                if !self.at(Tok::RBracket) {
                    loop {
                        elems.push(self.parse_expr()?);
                        if !self.eat(Tok::Comma) {
                            break;
                        }
                    }
                }
                let close = self.expect(Tok::RBracket)?.span;
                let span = start.to(close);
                Ok(Expr::Array(elems, span))
            }
            Tok::LBrace => self.parse_aggregate_lit(),
            _ => Err(self.err_here(format!(
                "expected an expression, found `{}`",
                self.peek().text()
            ))),
        }
    }

    /// Parse `{ field = expr, ... }` aggregate initializers.
    fn parse_aggregate_lit(&mut self) -> Result<Expr, CompileError> {
        let start = self.expect(Tok::LBrace)?.span;
        let mut fields = Vec::new();
        while !self.at(Tok::RBrace) {
            let fs = self.peek().span;
            let fname = self.expect_ident()?;
            self.expect(Tok::Assign)?;
            let value = self.parse_expr()?;
            let fe = value.span();
            fields.push((fname, value, fs.to(fe)));
            if !self.eat(Tok::Comma) {
                break;
            }
        }
        let close = self.expect(Tok::RBrace)?.span;
        let span = start.to(close);
        Ok(Expr::AggregateLit { fields, span })
    }

    /// Recognize the intrinsic forms `cast<T>(x)`, `load<T>(a)`,
    /// `store<T>(a, v)`, `sizeof(T)`, `alignof(T)`, `offsetof(T, f)`,
    /// `addr_of(x)`, and `trap(...)`. Returns `Ok(None)` when `name` is an
    /// ordinary identifier.
    fn try_parse_intrinsic(&mut self, name: &str) -> Result<Option<Expr>, CompileError> {
        let generic = matches!(name, "cast" | "load" | "store");
        let typed = matches!(name, "sizeof" | "alignof" | "offsetof");
        let plain = matches!(name, "addr_of" | "trap");
        if !generic && !typed && !plain {
            return Ok(None);
        }
        // `cast`/`load`/`store` require `<`; `sizeof`/`alignof`/`offsetof`
        // require `(`; `addr_of`/`trap` require `(`.
        if generic && !matches!(self.nth_kind(1), Some(Tok::Lt)) {
            return Err(self.err_here(format!("expected `<type>` after `{name}`")));
        }
        if typed && !matches!(self.nth_kind(1), Some(Tok::LParen)) {
            return Err(self.err_here(format!("expected `(type)` after `{name}`")));
        }
        if plain && !matches!(self.nth_kind(1), Some(Tok::LParen)) {
            return Ok(None);
        }

        let start = self.advance().span;

        if name == "addr_of" {
            self.expect(Tok::LParen)?;
            let expr = self.parse_expr()?;
            let close = self.expect(Tok::RParen)?.span;
            return Ok(Some(Expr::Unary {
                op: UnOp::AddrOf,
                expr: Box::new(expr),
                span: start.to(close),
            }));
        }
        if name == "trap" {
            return Err(
                self.err_here("`trap` is not implemented yet; trap identifiers are target-defined")
            );
        }

        let ty = if generic {
            self.expect(Tok::Lt)?;
            let ty = self.parse_type()?;
            if matches!(ty, Type::Void) {
                return Err(self.err_here("`void` is not a valid type argument"));
            }
            self.expect(Tok::Gt)?;
            ty
        } else {
            self.expect(Tok::LParen)?;
            let ty = self.parse_type()?;
            if matches!(ty, Type::Void) {
                return Err(self.err_here("`void` is not a valid type argument"));
            }
            if name == "offsetof" {
                self.expect(Tok::Comma)?;
            } else {
                self.expect(Tok::RParen)?;
            }
            ty
        };

        match name {
            "cast" => {
                self.expect(Tok::LParen)?;
                let expr = self.parse_expr()?;
                let close = self.expect(Tok::RParen)?.span;
                Ok(Some(Expr::Cast {
                    expr: Box::new(expr),
                    ty,
                    span: start.to(close),
                }))
            }
            "load" => {
                self.expect(Tok::LParen)?;
                let addr = self.parse_expr()?;
                let close = self.expect(Tok::RParen)?.span;
                Ok(Some(Expr::Load {
                    ty,
                    addr: Box::new(addr),
                    span: start.to(close),
                }))
            }
            "store" => {
                self.expect(Tok::LParen)?;
                let addr = self.parse_expr()?;
                self.expect(Tok::Comma)?;
                let value = self.parse_expr()?;
                let close = self.expect(Tok::RParen)?.span;
                Ok(Some(Expr::Store {
                    ty,
                    addr: Box::new(addr),
                    value: Box::new(value),
                    span: start.to(close),
                }))
            }
            "sizeof" | "alignof" => {
                let close = self.previous().span;
                Ok(Some(Expr::SizeOf {
                    ty,
                    align: name == "alignof",
                    span: start.to(close),
                }))
            }
            "offsetof" => {
                let field = self.expect_ident()?;
                let close = self.expect(Tok::RParen)?.span;
                Ok(Some(Expr::OffsetOf {
                    ty,
                    field,
                    span: start.to(close),
                }))
            }
            _ => unreachable!(),
        }
    }
}

fn is_lvalue(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::Ident(..)
            | Expr::Index { .. }
            | Expr::Field { .. }
            | Expr::Unary {
                op: UnOp::Deref,
                ..
            }
    )
}

/// Primitive scalar types recognised in type position. `int` is an alias for
/// `i32`; `void` is handled separately where relevant.
fn primitive_type(name: &str) -> Option<Type> {
    Some(match name {
        "bool" => Type::Bool,
        "i8" => Type::I8,
        "i16" => Type::I16,
        "i32" => Type::I32,
        "i64" => Type::I64,
        "u8" => Type::U8,
        "u16" => Type::U16,
        "u32" => Type::U32,
        "u64" => Type::U64,
        "word" => Type::Word,
        "sword" => Type::Sword,
        "int" => Type::I32,
        _ => return None,
    })
}

fn primitive_type_int(ty: &Type) -> Option<Type> {
    match ty {
        Type::I8
        | Type::I16
        | Type::I32
        | Type::I64
        | Type::U8
        | Type::U16
        | Type::U32
        | Type::U64
        | Type::Word
        | Type::Sword => Some(ty.clone()),
        _ => None,
    }
}

trait TokenText {
    fn token(&self) -> String;
    fn text(&self) -> String;
}

impl TokenText for Tok {
    fn token(&self) -> String {
        let s = match self {
            Tok::Int(_) => "integer",
            Tok::Str(_) => "string",
            Tok::Ident(_) => "identifier",
            Tok::Kw(kw) => match kw {
                Kw::If => "`if`",
                Kw::Else => "`else`",
                Kw::While => "`while`",
                Kw::For => "`for`",
                Kw::Break => "`break`",
                Kw::Continue => "`continue`",
                Kw::Return => "`return`",
                Kw::True => "`true`",
                Kw::False => "`false`",
                Kw::Static => "`static`",
                Kw::Stack => "`stack`",
                Kw::Entry => "`entry`",
                Kw::Null => "`null`",
                Kw::Type => "`type`",
                Kw::Struct => "`struct`",
                Kw::Enum => "`enum`",
                Kw::Void => "`void`",
            },
            Tok::LParen => "`(`",
            Tok::RParen => "`)`",
            Tok::LBrace => "`{`",
            Tok::RBrace => "`}`",
            Tok::LBracket => "`[`",
            Tok::RBracket => "`]`",
            Tok::Comma => "`,`",
            Tok::Semi => "`;`",
            Tok::Colon => "`:`",
            Tok::Dot => "`.`",
            Tok::Arrow => "`->`",
            Tok::Assign => "`=`",
            Tok::Plus => "`+`",
            Tok::Minus => "`-`",
            Tok::Star => "`*`",
            Tok::Slash => "`/`",
            Tok::Percent => "`%`",
            Tok::AndAnd => "`&&`",
            Tok::OrOr => "`||`",
            Tok::Amp => "`&`",
            Tok::Pipe => "`|`",
            Tok::Caret => "`^`",
            Tok::Tilde => "`~`",
            Tok::Bang => "`!`",
            Tok::Shl => "`<<`",
            Tok::Shr => "`>>`",
            Tok::Eq => "`==`",
            Tok::Ne => "`!=`",
            Tok::Lt => "`<`",
            Tok::Le => "`<=`",
            Tok::Gt => "`>`",
            Tok::Ge => "`>=`",
            Tok::Eof => "end of file",
        };
        s.to_string()
    }

    fn text(&self) -> String {
        match self {
            Tok::Int(v) => v.to_string(),
            Tok::Str(b) => String::from_utf8_lossy(b).to_string(),
            Tok::Ident(name) => name.clone(),
            other => other.token(),
        }
    }
}

impl TokenText for Token {
    fn token(&self) -> String {
        self.kind.token()
    }

    fn text(&self) -> String {
        self.kind.text()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_source;

    fn stmts_of(src: &str) -> Vec<Stmt> {
        let program = parse_source(src).unwrap();
        match &program.items[0] {
            Item::Func(func) => func.body.stmts.clone(),
            _ => panic!("expected a function"),
        }
    }

    #[test]
    fn parses_skeleton_program() {
        let src = r#"
            void putc(u8 c) { write(1, &c, 1); }

            static u32 counter = 0;

            u32 main() {
                u8 b[4] = [0, 0, 0, 0];
                b[0] = 0x2a;
                u32 x = fact(5);
                if (x == 120) { puts("ok\n"); } else { puts("bad\n"); }
                return 0;
            }
        "#;
        let program = parse_source(src).unwrap();
        assert_eq!(program.items.len(), 3);
        assert!(
            matches!(&program.items[0], Item::Func(f) if f.name == "putc"
                && f.params.len() == 1
                && matches!(f.ret_ty, Type::Void)
                && matches!(f.params[0].ty, Type::U8))
        );
        assert!(
            matches!(&program.items[1], Item::Global(g) if g.name == "counter"
                && matches!(g.ty, Type::U32)
                && g.explicit_static)
        );
        assert!(
            matches!(&program.items[2], Item::Func(f) if f.name == "main"
                && matches!(f.ret_ty, Type::U32))
        );
    }

    #[test]
    fn precedence_and_left_associativity() {
        let stmts = stmts_of("void main() { u32 x = 1 + 2 * 3; }");
        let Stmt::Decl {
            value: Some(value), ..
        } = &stmts[0]
        else {
            panic!()
        };
        assert!(matches!(value, Expr::Binary { op: BinOp::Add, .. }));
        assert!(matches!(
            value,
            Expr::Binary { rhs, .. } if matches!(**rhs, Expr::Binary { op: BinOp::Mul, .. })
        ));

        let stmts = stmts_of("void main() { u32 x = 10 - 3 - 4; }");
        let Stmt::Decl {
            value: Some(value), ..
        } = &stmts[0]
        else {
            panic!()
        };
        assert!(matches!(
            value,
            Expr::Binary { lhs, .. } if matches!(**lhs, Expr::Binary { op: BinOp::Sub, .. })
        ));
    }

    #[test]
    fn unary_operators() {
        let stmts =
            stmts_of("void main() { u32 a = -x; bool b = !ok; u32 c = ~mask; *u32 d = &y; }");
        assert!(matches!(
            &stmts[0], Stmt::Decl { value: Some(value), .. } if matches!(value, Expr::Unary { op: UnOp::Neg, .. })
        ));
        assert!(matches!(
            &stmts[1], Stmt::Decl { value: Some(value), .. } if matches!(value, Expr::Unary { op: UnOp::Not, .. })
        ));
        assert!(matches!(
            &stmts[2], Stmt::Decl { value: Some(value), .. } if matches!(value, Expr::Unary { op: UnOp::BitNot, .. })
        ));
        let Stmt::Decl { ty, .. } = &stmts[3] else {
            panic!()
        };
        assert!(matches!(ty, Type::Ptr(inner) if matches!(**inner, Type::U32)));
    }

    #[test]
    fn call_index_field() {
        let stmts = stmts_of("void main() { u32 a = f(x, y); u32 b = a[1]; }");
        assert!(matches!(
            &stmts[0], Stmt::Decl { value: Some(value), .. } if matches!(value, Expr::Call { args, .. } if args.len() == 2)
        ));
        let Stmt::Decl {
            value: Some(value), ..
        } = &stmts[1]
        else {
            panic!()
        };
        assert!(matches!(
            value,
            Expr::Index { base, index, .. } if matches!(**base, Expr::Ident(ref n, _) if n == "a")
                && matches!(**index, Expr::Int(1, _))
        ));
    }

    #[test]
    fn if_else_chain() {
        let stmts = stmts_of("void main() { if (a) { } else if (b) { } else { } }");
        let Stmt::If {
            cond,
            then_branch,
            else_branch,
            ..
        } = &stmts[0]
        else {
            panic!()
        };
        assert!(matches!(cond, Expr::Ident(..)));
        assert!(then_branch.stmts.is_empty());
        let else_block = else_branch.as_ref().expect("else present");
        assert_eq!(else_block.stmts.len(), 1);
        assert!(matches!(&else_block.stmts[0], Stmt::If { .. }));
    }

    #[test]
    fn while_for_break_continue_and_return() {
        let stmts = stmts_of(
            "void main() { while (n > 0) { n = n - 1; continue; } for (u32 i = 0; i < 4; i = i + 1) { break; } return; }",
        );
        assert!(matches!(&stmts[0], Stmt::While { cond, body, .. }
                if matches!(cond, Expr::Binary { op: BinOp::Gt, .. })
                    && matches!(&body.stmts[1], Stmt::Continue(_))));
        let Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } = &stmts[1]
        else {
            panic!()
        };
        assert!(init.is_some());
        assert!(cond.is_some());
        assert!(step.is_some());
        assert!(matches!(&body.stmts[0], Stmt::Break(_)));
        assert!(matches!(&stmts[2], Stmt::Return { value: None, .. }));
    }

    #[test]
    fn assignment_and_lvalue_checks() {
        let stmts = stmts_of("void main() { x = 2; a[i] = 3; }");
        assert!(
            matches!(&stmts[0], Stmt::Assign { target, value, .. } if matches!(target, Expr::Ident(..)) && matches!(value, Expr::Int(2, _)))
        );
        assert!(
            matches!(&stmts[1], Stmt::Assign { target, .. } if matches!(target, Expr::Index { .. }))
        );
        assert!(parse_source("void main() { 1 = 2; }").is_err());
        assert!(parse_source("void main() { (a + b) = 3; }").is_err());
    }

    #[test]
    fn array_types_and_literals() {
        let stmts = stmts_of("void main() { u8 b[4] = []; u32 c = [1, 2, 3]; }");
        let Stmt::Decl { ty, .. } = &stmts[0] else {
            panic!()
        };
        assert!(matches!(ty, Type::Array { elem, len: 4 } if matches!(**elem, Type::U8)));
        assert!(matches!(
            &stmts[1], Stmt::Decl { value: Some(value), .. } if matches!(value, Expr::Array(elems, _) if elems.len() == 3)
        ));
    }

    #[test]
    fn globals_and_static() {
        let program = parse_source("static u32 g = 7; u32 h; void main() {}").unwrap();
        assert!(
            matches!(&program.items[0], Item::Global(g) if g.name == "g" && g.explicit_static && g.value.is_some())
        );
        assert!(
            matches!(&program.items[1], Item::Global(g) if g.name == "h" && !g.explicit_static && g.value.is_none())
        );
    }

    #[test]
    fn prefix_pointer_declarators() {
        let stmts = stmts_of(
            "void main() { *u32 p; **u32 pp; u8 buf[4]; stack u8 page[16]; *u8 bytes = null; }",
        );
        let Stmt::Decl { ty, .. } = &stmts[0] else {
            panic!()
        };
        assert!(matches!(ty, Type::Ptr(inner) if matches!(**inner, Type::U32)));
        let Stmt::Decl { ty, .. } = &stmts[1] else {
            panic!()
        };
        let Type::Ptr(a) = ty else {
            panic!("expected a pointer")
        };
        let Type::Ptr(b) = &**a else {
            panic!("expected a pointer to pointer")
        };
        assert!(matches!(&**b, Type::U32));
        let Stmt::Decl { ty, .. } = &stmts[2] else {
            panic!()
        };
        assert!(matches!(ty, Type::Array { elem, len: 4 } if matches!(**elem, Type::U8)));
        let Stmt::Decl { storage, .. } = &stmts[3] else {
            panic!()
        };
        assert_eq!(*storage, Storage::Stack);
        assert!(matches!(
            &stmts[4],
            Stmt::Decl {
                value: Some(Expr::Null(_)),
                ..
            }
        ));
    }

    #[test]
    fn cast_load_store_and_sizeof_intrinsics() {
        let stmts = stmts_of(
            "void main() { u8 s = cast<u8>(n); u32 v = load<u32>(addr); store<u32>(addr, v); u32 n1 = sizeof(u32); u32 a = alignof(u64); }",
        );
        assert!(matches!(
            &stmts[0], Stmt::Decl { value: Some(Expr::Cast { ty, .. }), .. } if matches!(ty, Type::U8)));
        assert!(matches!(
            &stmts[1], Stmt::Decl { value: Some(Expr::Load { ty, .. }), .. } if matches!(ty, Type::U32)));
        assert!(matches!(&stmts[2], Stmt::Expr(Expr::Store { .. })));
        assert!(matches!(
            &stmts[3],
            Stmt::Decl {
                value: Some(Expr::SizeOf { align: false, .. }),
                ..
            }
        ));
        assert!(matches!(
            &stmts[4],
            Stmt::Decl {
                value: Some(Expr::SizeOf { align: true, .. }),
                ..
            }
        ));
    }

    #[test]
    fn offsetof_intrinsic() {
        let program = parse_source(
            "struct PacketHeader { u8 version; u16 length; }; void main() { u32 off = offsetof(PacketHeader, length); }",
        )
        .unwrap();
        let Item::Func(f) = &program.items[1] else {
            panic!("expected a function")
        };
        let Stmt::Decl {
            value: Some(Expr::OffsetOf { field, .. }),
            ..
        } = &f.body.stmts[0]
        else {
            panic!()
        };
        assert_eq!(field, "length");
    }

    #[test]
    fn struct_layout_packed_enum_underlying_and_aggregate_init() {
        let src = r#"
            struct PacketHeader layout(packed) {
                u8 version;
                u16 length;
            };
            enum ProcessState : u32 { Ready, Running = 5, Sleeping };
            type ProcessId = u32;
            entry start;
            void start() {
                PacketHeader hdr = { version = 1, length = 8 };
                ProcessState s = Ready;
            }
        "#;
        let program = parse_source(src).unwrap();
        assert!(matches!(&program.items[0], Item::Struct(s) if s.packed && s.fields.len() == 2));
        assert!(
            matches!(&program.items[1], Item::Enum(e) if matches!(e.underlying, Type::U32) && e.variants.len() == 3)
        );
        assert!(matches!(&program.items[2], Item::TypeAlias(a) if a.name == "ProcessId"));
        assert!(matches!(&program.items[3], Item::Entry(e) if e.name == "start"));
        let Item::Func(f) = &program.items[4] else {
            panic!()
        };
        let Stmt::Decl {
            value: Some(value), ..
        } = &f.body.stmts[0]
        else {
            panic!()
        };
        assert!(matches!(
            value,
            Expr::AggregateLit { fields, .. } if fields.len() == 2
        ));
        assert!(matches!(
            &f.body.stmts[1], Stmt::Decl { value: Some(Expr::Ident(name, _)), .. } if name == "Ready"));
    }

    #[test]
    fn reported_errors() {
        assert!(parse_source("void main() { u32 x = 1 }").is_err());
        assert!(parse_source("void main() { u32 x = 1;").is_err());
        assert!(parse_source("void main( {}").is_err());
        assert!(
            parse_source("void main() { if x { } }")
                .unwrap_err()
                .message
                .contains("expected ``(``")
        );
        assert!(parse_source("void main() { unsafe { } }").is_err());
        assert!(
            parse_source("void main() { u32 x = (u32)y; }").is_err(),
            "C-style casts are gone"
        );
        assert!(
            parse_source("void main() { trap(1); }")
                .unwrap_err()
                .message
                .contains("not implemented")
        );
    }
}
