//! The episode condition: a small boolean expression over RAM variables, such as
//! `lives == 0 && game_over != 0` (docs/games.md, "Episode").

use std::collections::BTreeMap;
use std::fmt;

/// A parsed condition. It keeps its source, which is what it serializes back to.
///
/// Grammar, loosest binding first; values are `i64`, and a value is true when it isn't 0.
/// A comparison takes no comparison as an operand (`a < b == c` is an error), and unary
/// operators and parentheses nest at most 32 deep, in at most 256 tokens:
///
/// ```text
/// or   := and ("||" and)*
/// and  := cmp ("&&" cmp)*
/// cmp  := unary (("==" | "!=" | "<" | "<=" | ">" | ">=") unary)?
/// unary := "!" unary | "-" unary | "(" or ")" | integer | variable
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Condition {
    source: String,
    expr: Expr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Expr {
    Int(i64),
    Var(String),
    Not(Box<Expr>),
    Neg(Box<Expr>),
    Cmp(Box<Expr>, Op, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Int(i64),
    Ident(String),
    Sym(&'static str),
}

const SYMBOLS: [&str; 13] = [
    "==", "!=", "<=", ">=", "&&", "||", "<", ">", "!", "-", "(", ")", "=",
];

/// How deep `!`, `-` and parentheses may nest, so a pathological condition can't overflow the
/// parser's stack.
const MAX_DEPTH: usize = 32;

/// The most tokens a condition may have, which also bounds the depth of its `&&` and `||`
/// chains when it is evaluated.
const MAX_TOKENS: usize = 256;

fn tokenize(source: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut rest = source.trim_start();
    while let Some(c) = rest.chars().next() {
        let len = if c.is_ascii_digit() {
            let len = rest
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(rest.len());
            let value = rest[..len]
                .parse()
                .map_err(|_| format!("{} is too large", &rest[..len]))?;
            tokens.push(Token::Int(value));
            len
        } else if c.is_ascii_alphabetic() || c == '_' {
            let len = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            tokens.push(Token::Ident(rest[..len].to_owned()));
            len
        } else {
            let symbol = SYMBOLS
                .into_iter()
                .find(|symbol| rest.starts_with(symbol))
                .ok_or_else(|| format!("unexpected {c:?}"))?;
            if symbol == "=" {
                return Err("`=` isn't an operator; compare with `==`".into());
            }
            tokens.push(Token::Sym(symbol));
            symbol.len()
        };
        rest = rest[len..].trim_start();
        if tokens.len() > MAX_TOKENS {
            return Err(format!("longer than {MAX_TOKENS} tokens"));
        }
    }
    Ok(tokens)
}

struct Parser {
    tokens: Vec<Token>,
    at: usize,
    depth: usize,
}

impl Parser {
    fn eat(&mut self, symbol: &str) -> bool {
        let found = matches!(self.tokens.get(self.at), Some(Token::Sym(s)) if *s == symbol);
        self.at += usize::from(found);
        found
    }

    fn or(&mut self) -> Result<Expr, String> {
        let mut expr = self.and()?;
        while self.eat("||") {
            expr = Expr::Or(Box::new(expr), Box::new(self.and()?));
        }
        Ok(expr)
    }

    fn and(&mut self) -> Result<Expr, String> {
        let mut expr = self.cmp()?;
        while self.eat("&&") {
            expr = Expr::And(Box::new(expr), Box::new(self.cmp()?));
        }
        Ok(expr)
    }

    fn cmp(&mut self) -> Result<Expr, String> {
        let left = self.unary()?;
        let ops = [
            ("==", Op::Eq),
            ("!=", Op::Ne),
            ("<=", Op::Le),
            (">=", Op::Ge),
            ("<", Op::Lt),
            (">", Op::Gt),
        ];
        for (symbol, op) in ops {
            if self.eat(symbol) {
                return Ok(Expr::Cmp(Box::new(left), op, Box::new(self.unary()?)));
            }
        }
        Ok(left)
    }

    /// Parses one nested level with `f`, failing past [`MAX_DEPTH`].
    fn nested(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<Expr, String>,
    ) -> Result<Expr, String> {
        if self.depth == MAX_DEPTH {
            return Err(format!("nested more than {MAX_DEPTH} deep"));
        }
        self.depth += 1;
        let expr = f(self);
        self.depth -= 1;
        expr
    }

    fn unary(&mut self) -> Result<Expr, String> {
        if self.eat("!") {
            return self.nested(|p| Ok(Expr::Not(Box::new(p.unary()?))));
        }
        if self.eat("-") {
            return self.nested(|p| Ok(Expr::Neg(Box::new(p.unary()?))));
        }
        if self.eat("(") {
            let expr = self.nested(Self::or)?;
            return if self.eat(")") {
                Ok(expr)
            } else {
                Err("missing `)`".into())
            };
        }
        let token = self.tokens.get(self.at).cloned();
        self.at += 1;
        match token {
            Some(Token::Int(value)) => Ok(Expr::Int(value)),
            Some(Token::Ident(name)) => Ok(Expr::Var(name)),
            Some(Token::Sym(symbol)) => Err(format!("unexpected `{symbol}`")),
            None => Err("unexpected end".into()),
        }
    }
}

impl Condition {
    /// Parses `source`; the error says what is wrong. Variable names aren't checked here
    /// ([`variables`](Self::variables) lists them for the caller to check).
    pub fn parse(source: &str) -> Result<Condition, String> {
        let mut parser = Parser {
            tokens: tokenize(source)?,
            at: 0,
            depth: 0,
        };
        let expr = parser.or()?;
        if let Some(token) = parser.tokens.get(parser.at) {
            return Err(format!("unexpected {token:?} after a complete condition"));
        }
        Ok(Condition {
            source: source.to_owned(),
            expr,
        })
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    /// The variables the condition reads, sorted, each once.
    pub fn variables(&self) -> Vec<&str> {
        fn walk<'a>(expr: &'a Expr, out: &mut Vec<&'a str>) {
            match expr {
                Expr::Int(_) => {}
                Expr::Var(name) => out.push(name),
                Expr::Not(inner) | Expr::Neg(inner) => walk(inner, out),
                Expr::Cmp(a, _, b) | Expr::And(a, b) | Expr::Or(a, b) => {
                    walk(a, out);
                    walk(b, out);
                }
            }
        }
        let mut names = Vec::new();
        walk(&self.expr, &mut names);
        names.sort_unstable();
        names.dedup();
        names
    }

    /// Evaluates the condition on variable values. Fails, naming it, if a variable is missing.
    pub fn eval(&self, values: &BTreeMap<String, i64>) -> Result<bool, String> {
        fn value(expr: &Expr, values: &BTreeMap<String, i64>) -> Result<i64, String> {
            Ok(match expr {
                Expr::Int(n) => *n,
                Expr::Var(name) => *values
                    .get(name)
                    .ok_or_else(|| format!("no value for RAM variable {name:?}"))?,
                Expr::Not(inner) => i64::from(value(inner, values)? == 0),
                // RAM values and literals are never negative, so this can't overflow; wrapping
                // keeps eval total anyway.
                Expr::Neg(inner) => value(inner, values)?.wrapping_neg(),
                Expr::Cmp(a, op, b) => {
                    let (a, b) = (value(a, values)?, value(b, values)?);
                    i64::from(match op {
                        Op::Eq => a == b,
                        Op::Ne => a != b,
                        Op::Lt => a < b,
                        Op::Le => a <= b,
                        Op::Gt => a > b,
                        Op::Ge => a >= b,
                    })
                }
                // Short-circuits, like the operators it borrows.
                Expr::And(a, b) => i64::from(value(a, values)? != 0 && value(b, values)? != 0),
                Expr::Or(a, b) => i64::from(value(a, values)? != 0 || value(b, values)? != 0),
            })
        }
        Ok(value(&self.expr, values)? != 0)
    }
}

impl fmt::Display for Condition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.source)
    }
}

impl serde::Serialize for Condition {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.source)
    }
}

impl<'de> serde::Deserialize<'de> for Condition {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let source = String::deserialize(deserializer)?;
        Condition::parse(&source)
            .map_err(|reason| serde::de::Error::custom(format!("condition {source:?}: {reason}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(source: &str, values: &[(&str, i64)]) -> bool {
        let values = values.iter().map(|&(k, v)| (k.to_owned(), v)).collect();
        Condition::parse(source).unwrap().eval(&values).unwrap()
    }

    #[test]
    fn evaluates_with_precedence() {
        let game_over = [("lives", 0), ("flag", 1)];
        assert!(eval("lives == 0 && flag", &game_over));
        assert!(!eval("lives == 0 && !flag", &game_over));
        assert!(eval("lives > 0 || flag && lives <= 0", &game_over));
        assert!(!eval("(lives > 0 || flag) && lives != 0", &game_over));
        assert!(eval("!(lives >= 1)", &game_over));
        assert!(eval("lives<1&&flag==1", &game_over), "spaces are optional");
        assert!(eval("3 > 2", &[]));
        assert!(eval("lives > -1 && -lives == 0 && --flag", &game_over));
    }

    #[test]
    fn lists_variables_and_reports_missing_ones() {
        let condition = Condition::parse("lives == 0 && (flag || lives < stage)").unwrap();
        assert_eq!(condition.variables(), ["flag", "lives", "stage"]);
        let error = condition.eval(&BTreeMap::new()).unwrap_err();
        assert!(error.contains("\"lives\""), "{error}");
    }

    #[test]
    fn rejects_malformed_conditions() {
        for (source, reason) in [
            ("", "unexpected end"),
            ("lives = 0", "`==`"),
            ("lives == 0 &&", "unexpected end"),
            ("(lives == 0", "missing `)`"),
            ("lives == 0 lives", "after a complete condition"),
            ("lives # 2", "unexpected '#'"),
            ("99999999999999999999 > 0", "too large"),
            ("lives == == 0", "unexpected `==`"),
            ("lives < 1 == flag", "after a complete condition"),
            (
                format!("{}lives", "!".repeat(MAX_DEPTH + 1)).as_str(),
                "nested more than",
            ),
            (
                format!("{}lives{}", "(".repeat(40), ")".repeat(40)).as_str(),
                "nested more than",
            ),
            (
                vec!["lives"; 200].join(" && ").as_str(),
                "longer than 256 tokens",
            ),
        ] {
            let error = Condition::parse(source).unwrap_err();
            assert!(error.contains(reason), "{source:?}: {error}");
        }
    }
}
