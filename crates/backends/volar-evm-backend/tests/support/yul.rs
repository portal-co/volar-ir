//! Interpreter for exactly the whitelisted Yul subset `SolidityAsm` emits:
//! `let`, `:=`, `and`, `or`, `xor`, `not`, `mload`, `mstore`, `add`, and
//! numeric literals — no blocks, no control flow. Parsing doubles as the
//! whitelist lint: anything outside the subset is a parse error.

use std::collections::BTreeMap;

use super::word::{self, Word};

/// Post-run machine state: variable bindings and word-addressed memory.
pub struct Env {
    /// Variable bindings (`v*`, `inputs`, `outputs`).
    pub vars: BTreeMap<String, Word>,
    /// 32-byte-word memory, keyed by byte address. Unwritten cells read as
    /// zero, matching EVM semantics.
    pub mem: BTreeMap<u64, Word>,
}

/// The body text of the first `assembly { ... }` block in `src`. The
/// whitelist contains no nested blocks, so the first `}` after the opening
/// brace terminates the body.
pub fn extract_body(src: &str) -> &str {
    let start = src
        .find("assembly {")
        .expect("emitted source contains an assembly block")
        + "assembly {".len();
    let end = src[start..].find('}').expect("assembly block is closed") + start;
    &src[start..end]
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Let,
    Ident(String),
    Num(u64),
    Assign,
    LParen,
    RParen,
    Comma,
}

fn tokenize(body: &str) -> Result<Vec<Tok>, String> {
    let mut toks = Vec::new();
    let mut chars = body.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            c if c.is_whitespace() => {
                chars.next();
            }
            '(' => {
                chars.next();
                toks.push(Tok::LParen);
            }
            ')' => {
                chars.next();
                toks.push(Tok::RParen);
            }
            ',' => {
                chars.next();
                toks.push(Tok::Comma);
            }
            ':' => {
                chars.next();
                if chars.next() != Some('=') {
                    return Err("expected `:=`".into());
                }
                toks.push(Tok::Assign);
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                let mut name = String::new();
                while let Some(&c) = chars.peek() {
                    if !c.is_ascii_alphanumeric() && c != '_' {
                        break;
                    }
                    name.push(c);
                    chars.next();
                }
                if name == "let" {
                    toks.push(Tok::Let);
                } else {
                    toks.push(Tok::Ident(name));
                }
            }
            c if c.is_ascii_digit() => {
                let mut text = String::new();
                while let Some(&c) = chars.peek() {
                    if !c.is_ascii_alphanumeric() {
                        break;
                    }
                    text.push(c);
                    chars.next();
                }
                let value = if let Some(hex) = text.strip_prefix("0x") {
                    u64::from_str_radix(hex, 16).map_err(|e| e.to_string())?
                } else {
                    text.parse::<u64>().map_err(|e| e.to_string())?
                };
                toks.push(Tok::Num(value));
            }
            c => return Err(format!("unexpected character {c:?}")),
        }
    }
    Ok(toks)
}

#[derive(Debug)]
enum Expr {
    Num(u64),
    Var(String),
    Call(String, Vec<Expr>),
}

#[derive(Debug)]
enum Stmt {
    Bind(String, Expr),
    Store(Expr, Expr),
}

struct Parser<'a> {
    toks: &'a [Tok],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn next(&mut self) -> Option<&Tok> {
        let tok = self.toks.get(self.pos);
        self.pos += usize::from(tok.is_some());
        tok
    }

    fn expect(&mut self, want: &Tok) -> Result<(), String> {
        if self.next() == Some(want) {
            Ok(())
        } else {
            Err(format!("expected {want:?} at token {}", self.pos))
        }
    }

    fn ident(&mut self) -> Result<String, String> {
        match self.next() {
            Some(Tok::Ident(name)) => Ok(name.clone()),
            other => Err(format!("expected identifier, got {other:?}")),
        }
    }

    fn expr(&mut self) -> Result<Expr, String> {
        match self.next() {
            Some(Tok::Num(value)) => Ok(Expr::Num(*value)),
            Some(Tok::Ident(name)) => {
                let name = name.clone();
                if self.peek() == Some(&Tok::LParen) {
                    self.next();
                    let mut args = Vec::new();
                    if self.peek() != Some(&Tok::RParen) {
                        loop {
                            args.push(self.expr()?);
                            match self.next() {
                                Some(Tok::Comma) => {}
                                Some(Tok::RParen) => break,
                                other => return Err(format!("expected `,` or `)`, got {other:?}")),
                            }
                        }
                    } else {
                        self.next();
                    }
                    Ok(Expr::Call(name, args))
                } else {
                    Ok(Expr::Var(name))
                }
            }
            other => Err(format!("expected expression, got {other:?}")),
        }
    }

    fn stmt(&mut self) -> Result<Stmt, String> {
        if self.peek() == Some(&Tok::Let) {
            self.next();
            let name = self.ident()?;
            self.expect(&Tok::Assign)?;
            return Ok(Stmt::Bind(name, self.expr()?));
        }
        let name = self.ident()?;
        if self.peek() == Some(&Tok::Assign) {
            self.next();
            return Ok(Stmt::Bind(name, self.expr()?));
        }
        // A bare call statement: only `mstore` is whitelisted.
        if name != "mstore" {
            return Err(format!("unsupported statement `{name}(...)`"));
        }
        self.expect(&Tok::LParen)?;
        let addr = self.expr()?;
        self.expect(&Tok::Comma)?;
        let value = self.expr()?;
        self.expect(&Tok::RParen)?;
        Ok(Stmt::Store(addr, value))
    }
}

fn parse(body: &str) -> Result<Vec<Stmt>, String> {
    let toks = tokenize(body)?;
    let mut parser = Parser {
        toks: &toks,
        pos: 0,
    };
    let mut stmts = Vec::new();
    while parser.peek().is_some() {
        stmts.push(parser.stmt()?);
    }
    Ok(stmts)
}

/// The whitelist lint: parse `body`, rejecting anything outside the subset.
// Only the Solidity test binary uses this; each test binary compiles the
// support module independently.
#[allow(dead_code)]
pub fn check(body: &str) -> Result<(), String> {
    parse(body).map(|_| ())
}

fn eval(expr: &Expr, env: &Env) -> Result<Word, String> {
    match expr {
        Expr::Num(value) => Ok(word::from_u64(*value)),
        Expr::Var(name) => env
            .vars
            .get(name)
            .copied()
            .ok_or_else(|| format!("unbound variable `{name}`")),
        Expr::Call(name, args) => {
            let values: Vec<Word> = args
                .iter()
                .map(|arg| eval(arg, env))
                .collect::<Result<_, _>>()?;
            let binary = |f: fn(Word, Word) -> Word| -> Result<Word, String> {
                if values.len() != 2 {
                    return Err(format!("`{name}` expects 2 arguments"));
                }
                Ok(f(values[0], values[1]))
            };
            match name.as_str() {
                "and" => binary(word::and),
                "or" => binary(word::or),
                "xor" => binary(word::xor),
                "add" => binary(word::add),
                "not" => {
                    if values.len() != 1 {
                        return Err("`not` expects 1 argument".into());
                    }
                    Ok(word::not(values[0]))
                }
                "mload" => {
                    if values.len() != 1 {
                        return Err("`mload` expects 1 argument".into());
                    }
                    let addr = word::as_u64(values[0]).ok_or("`mload` address exceeds 64 bits")?;
                    Ok(env.mem.get(&addr).copied().unwrap_or(word::ZERO))
                }
                _ => Err(format!("unsupported builtin `{name}`")),
            }
        }
    }
}

/// Run `body` (as extracted by [`extract_body`]) with the `inputs` memory
/// array placed at `inputs_ptr` and the free-memory pointer (`0x40`) set to
/// `free_ptr`. Returns the final variable bindings and memory.
pub fn run(body: &str, inputs_ptr: u64, inputs: &[Word], free_ptr: u64) -> Result<Env, String> {
    let mut env = Env {
        vars: BTreeMap::new(),
        mem: BTreeMap::new(),
    };
    env.vars.insert("inputs".into(), word::from_u64(inputs_ptr));
    env.mem.insert(0x40, word::from_u64(free_ptr));
    env.mem
        .insert(inputs_ptr, word::from_u64(inputs.len() as u64));
    for (index, value) in inputs.iter().enumerate() {
        env.mem
            .insert(inputs_ptr + 0x20 * (index as u64 + 1), *value);
    }

    for stmt in parse(body)? {
        match stmt {
            Stmt::Bind(name, expr) => {
                let value = eval(&expr, &env)?;
                env.vars.insert(name, value);
            }
            Stmt::Store(addr, value) => {
                let addr = eval(&addr, &env)?;
                let value = eval(&value, &env)?;
                let addr = word::as_u64(addr).ok_or("`mstore` address exceeds 64 bits")?;
                env.mem.insert(addr, value);
            }
        }
    }
    Ok(env)
}
