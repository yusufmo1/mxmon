//! A small tri-state boolean language over a report value, for `mxmon check`.
//!
//! Grammar (paths use the same dot syntax as `get`):
//! ```text
//! expr    := or
//! or      := and ("or" and)*
//! and     := cmp ("and" cmp)*
//! cmp     := "not" cmp | "(" expr ")" | operand (op operand)?
//! op      := "<" | "<=" | ">" | ">=" | "==" | "!="
//! operand := path | number | string | "true" | "false" | "null"
//! ```
//!
//! Evaluation is tri-state. An operand path that resolves to `null` (a source
//! that is down or disabled) makes an ordering or a non-null equality
//! `Unknown` rather than silently `False`, so `thermal.cpu_max_c < 90` never
//! passes just because the temperature source was unavailable.

use serde_json::Value;

use super::select::{self, Seg};

/// The outcome of an evaluation.
#[derive(Debug, PartialEq)]
pub enum Verdict {
    True,
    False,
    /// The expression could not be decided because a referenced source was
    /// null; carries a short reason.
    Unknown(String),
}

/// Evaluate `expr` against `root`. `Err` means the expression is malformed (a
/// distinct outcome from `Unknown`).
pub fn evaluate(root: &Value, expr: &str) -> Result<Verdict, String> {
    let tokens = tokenize(expr)?;
    let mut p = Parser {
        tokens,
        pos: 0,
        root,
    };
    let v = p.expr()?;
    if p.pos != p.tokens.len() {
        return Err(format!("unexpected trailing input at token {}", p.pos));
    }
    Ok(v)
}

// ---- tokens --------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Num(f64),
    Str(String),
    Op(String),
    And,
    Or,
    Not,
    True,
    False,
    Null,
    LParen,
    RParen,
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '[' | ']')
}

fn tokenize(s: &str) -> Result<Vec<Tok>, String> {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        match c {
            '(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            '"' | '\'' => {
                let quote = c;
                i += 1;
                let start = i;
                while i < chars.len() && chars[i] != quote {
                    i += 1;
                }
                if i >= chars.len() {
                    return Err("unterminated string literal".to_owned());
                }
                out.push(Tok::Str(chars[start..i].iter().collect()));
                i += 1;
            }
            '<' | '>' | '=' | '!' => {
                let two: String = chars[i..(i + 2).min(chars.len())].iter().collect();
                if matches!(two.as_str(), "<=" | ">=" | "==" | "!=") {
                    out.push(Tok::Op(two));
                    i += 2;
                } else if c == '<' || c == '>' {
                    out.push(Tok::Op(c.to_string()));
                    i += 1;
                } else {
                    return Err(format!("expected '=' after {c:?}"));
                }
            }
            c if c.is_ascii_digit() || c == '-' => {
                let start = i;
                i += 1;
                while i < chars.len()
                    && (chars[i].is_ascii_digit()
                        || matches!(chars[i], '.' | 'e' | 'E' | '+' | '-'))
                {
                    i += 1;
                }
                let lit: String = chars[start..i].iter().collect();
                let n = lit
                    .parse::<f64>()
                    .map_err(|_| format!("bad number {lit:?}"))?;
                out.push(Tok::Num(n));
            }
            c if is_ident_char(c) => {
                let start = i;
                while i < chars.len() && is_ident_char(chars[i]) {
                    i += 1;
                }
                let w: String = chars[start..i].iter().collect();
                out.push(match w.as_str() {
                    "and" => Tok::And,
                    "or" => Tok::Or,
                    "not" => Tok::Not,
                    "true" => Tok::True,
                    "false" => Tok::False,
                    "null" => Tok::Null,
                    _ => Tok::Ident(w),
                });
            }
            other => return Err(format!("unexpected character {other:?}")),
        }
    }
    Ok(out)
}

// ---- operands ------------------------------------------------------------

/// A resolved operand plus the two facts equality/ordering need: whether it is
/// a path that came back `null` (unavailable), and whether it is the literal
/// `null` (an explicit availability check).
struct OpVal {
    value: Value,
    unavailable: bool,
    literal_null: bool,
}

enum Operand {
    Path(Vec<Seg>),
    Lit(Value),
    LitNull,
}

// ---- parser --------------------------------------------------------------

struct Parser<'a> {
    tokens: Vec<Tok>,
    pos: usize,
    root: &'a Value,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos)
    }

    fn bump(&mut self) -> Option<Tok> {
        let t = self.tokens.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn expr(&mut self) -> Result<Verdict, String> {
        self.or()
    }

    fn or(&mut self) -> Result<Verdict, String> {
        let mut v = self.and()?;
        while matches!(self.peek(), Some(Tok::Or)) {
            self.bump();
            let rhs = self.and()?;
            v = combine_or(v, rhs);
        }
        Ok(v)
    }

    fn and(&mut self) -> Result<Verdict, String> {
        let mut v = self.cmp()?;
        while matches!(self.peek(), Some(Tok::And)) {
            self.bump();
            let rhs = self.cmp()?;
            v = combine_and(v, rhs);
        }
        Ok(v)
    }

    fn cmp(&mut self) -> Result<Verdict, String> {
        if matches!(self.peek(), Some(Tok::Not)) {
            self.bump();
            return Ok(negate(self.cmp()?));
        }
        if matches!(self.peek(), Some(Tok::LParen)) {
            self.bump();
            let v = self.expr()?;
            match self.bump() {
                Some(Tok::RParen) => return Ok(v),
                _ => return Err("expected ')'".to_owned()),
            }
        }
        let lhs = self.operand()?;
        if let Some(Tok::Op(op)) = self.peek().cloned() {
            self.bump();
            let rhs = self.operand()?;
            let l = self.resolve(lhs)?;
            let r = self.resolve(rhs)?;
            return Ok(eval_cmp(&op, &l, &r));
        }
        // A bare operand is read as a boolean.
        let v = self.resolve(lhs)?;
        if v.unavailable {
            return Ok(Verdict::Unknown(
                "operand is null (source unavailable)".to_owned(),
            ));
        }
        match v.value.as_bool() {
            Some(true) => Ok(Verdict::True),
            Some(false) => Ok(Verdict::False),
            None => Ok(Verdict::Unknown("operand is not a boolean".to_owned())),
        }
    }

    fn operand(&mut self) -> Result<Operand, String> {
        match self.bump() {
            Some(Tok::Ident(path)) => Ok(Operand::Path(select::parse_path(&path)?)),
            Some(Tok::Num(n)) => Ok(Operand::Lit(
                serde_json::Number::from_f64(n).map_or(Value::Null, Value::Number),
            )),
            Some(Tok::Str(s)) => Ok(Operand::Lit(Value::String(s))),
            Some(Tok::True) => Ok(Operand::Lit(Value::Bool(true))),
            Some(Tok::False) => Ok(Operand::Lit(Value::Bool(false))),
            Some(Tok::Null) => Ok(Operand::LitNull),
            None => Err("unexpected end of expression".to_owned()),
            Some(tok) => Err(format!("expected an operand, found {tok:?}")),
        }
    }

    fn resolve(&self, op: Operand) -> Result<OpVal, String> {
        Ok(match op {
            Operand::Path(segs) => {
                let value = select::resolve(self.root, &segs)?.clone();
                let unavailable = value.is_null();
                OpVal {
                    value,
                    unavailable,
                    literal_null: false,
                }
            }
            Operand::Lit(value) => OpVal {
                value,
                unavailable: false,
                literal_null: false,
            },
            Operand::LitNull => OpVal {
                value: Value::Null,
                unavailable: false,
                literal_null: true,
            },
        })
    }
}

fn eval_cmp(op: &str, l: &OpVal, r: &OpVal) -> Verdict {
    let literal_null = l.literal_null || r.literal_null;
    match op {
        "==" | "!=" => {
            // An unavailable path is undecidable unless the user is explicitly
            // testing for null (e.g. `thermal == null`).
            if (l.unavailable || r.unavailable) && !literal_null {
                return Verdict::Unknown("operand is null (source unavailable)".to_owned());
            }
            let eq = json_eq(&l.value, &r.value);
            bool_verdict(if op == "==" { eq } else { !eq })
        }
        _ => {
            if l.unavailable || r.unavailable || l.value.is_null() || r.value.is_null() {
                return Verdict::Unknown("ordering comparison with a null operand".to_owned());
            }
            match (l.value.as_f64(), r.value.as_f64()) {
                (Some(a), Some(b)) => bool_verdict(match op {
                    "<" => a < b,
                    "<=" => a <= b,
                    ">" => a > b,
                    _ => a >= b,
                }),
                _ => Verdict::Unknown("non-numeric ordering comparison".to_owned()),
            }
        }
    }
}

fn json_eq(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => (x - y).abs() < f64::EPSILON,
        _ => a == b,
    }
}

fn bool_verdict(b: bool) -> Verdict {
    if b { Verdict::True } else { Verdict::False }
}

fn negate(v: Verdict) -> Verdict {
    match v {
        Verdict::True => Verdict::False,
        Verdict::False => Verdict::True,
        Verdict::Unknown(r) => Verdict::Unknown(r),
    }
}

fn combine_and(a: Verdict, b: Verdict) -> Verdict {
    match (a, b) {
        (Verdict::False, _) | (_, Verdict::False) => Verdict::False,
        (Verdict::Unknown(r), _) | (_, Verdict::Unknown(r)) => Verdict::Unknown(r),
        (Verdict::True, Verdict::True) => Verdict::True,
    }
}

fn combine_or(a: Verdict, b: Verdict) -> Verdict {
    match (a, b) {
        (Verdict::True, _) | (_, Verdict::True) => Verdict::True,
        (Verdict::Unknown(r), _) | (_, Verdict::Unknown(r)) => Verdict::Unknown(r),
        (Verdict::False, Verdict::False) => Verdict::False,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn report() -> Value {
        json!({
            "power": {"package_w": 12.0},
            "thermal": {"cpu_max_c": 58.5, "throttling": false},
            "memory": {"pressure": "normal"},
            "ping": null,
        })
    }

    #[test]
    fn comparisons_and_logic() {
        let r = report();
        assert_eq!(
            evaluate(&r, "thermal.cpu_max_c < 90").unwrap(),
            Verdict::True
        );
        assert_eq!(
            evaluate(&r, "power.package_w > 40").unwrap(),
            Verdict::False
        );
        assert_eq!(
            evaluate(&r, "memory.pressure == \"normal\"").unwrap(),
            Verdict::True
        );
        assert_eq!(
            evaluate(&r, "thermal.throttling == false and power.package_w < 40").unwrap(),
            Verdict::True
        );
        assert_eq!(
            evaluate(&r, "not thermal.throttling").unwrap(),
            Verdict::True
        );
    }

    #[test]
    fn null_operand_is_unknown_not_false() {
        let r = report();
        // ping is null (disabled); ordering/equality against a real value is
        // undecidable, not false.
        assert!(matches!(
            evaluate(&r, "ping.rtt_ms < 50").unwrap(),
            Verdict::Unknown(_)
        ));
        assert!(matches!(
            evaluate(&r, "ping.up == true").unwrap(),
            Verdict::Unknown(_)
        ));
        // But an explicit availability check decides.
        assert_eq!(evaluate(&r, "ping == null").unwrap(), Verdict::True);
    }

    #[test]
    fn malformed_is_err_distinct_from_unknown() {
        let r = report();
        assert!(evaluate(&r, "power.package_w <").is_err());
        assert!(evaluate(&r, "&& nonsense").is_err());
        // A path not in the schema is a malformed operand at eval time.
        assert!(evaluate(&r, "power.bogus < 1").is_err());
    }

    mod prop {
        use super::super::evaluate;
        use proptest::prelude::*;
        use serde_json::json;

        proptest! {
            #[test]
            fn evaluate_never_panics(s in ".*") {
                let r = json!({"a": {"b": 1.0}, "c": null});
                let _ = evaluate(&r, &s);
            }
        }
    }
}
