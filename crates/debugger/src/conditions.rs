//! What a breakpoint asks before it stops: a condition (`count > 3 &&
//! order.id == "A-1"`), a hit count (`>= 5`, `% 10`), or a message to log
//! instead of stopping (`sum is {sum}`).
//!
//! A condition compares paths — what `Session::evaluate` takes — and
//! literals with `== != < <= > >=`, joined by `&& || !` and parentheses.
//! Nothing in it calls the program's code.

/// A value a condition compares.
#[derive(Clone, PartialEq, Debug)]
pub enum Operand {
    Null,
    Boolean(bool),
    Integer(i64),
    Decimal(f64),
    Char(char),
    Text(String),
    /// Any other object, by its id: equal only to itself.
    Reference(u64),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Comparison {
    Equal,
    NotEqual,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
}

/// A parsed condition.
#[derive(Clone, PartialEq, Debug)]
pub enum Condition {
    Or(Box<Condition>, Box<Condition>),
    And(Box<Condition>, Box<Condition>),
    Not(Box<Condition>),
    Compare(Term, Comparison, Term),
    /// A path or literal that must be a boolean.
    Holds(Term),
}

#[derive(Clone, PartialEq, Debug)]
pub enum Term {
    Literal(Operand),
    /// `order.items[0].price`.
    Path(String),
}

impl Condition {
    pub fn parse(text: &str) -> Result<Condition, String> {
        let tokens = tokens(text)?;
        let mut parser = Parser {
            tokens: &tokens,
            position: 0,
        };
        let condition = parser.or()?;
        match parser.tokens.get(parser.position) {
            None => Ok(condition),
            Some(token) => Err(format!("unexpected {token:?} in `{text}`")),
        }
    }

    /// Whether the condition holds, a path's value coming from
    /// `resolve`.
    pub fn holds(
        &self,
        resolve: &mut dyn FnMut(&str) -> Result<Operand, String>,
    ) -> Result<bool, String> {
        match self {
            Condition::Or(left, right) => Ok(left.holds(resolve)? || right.holds(resolve)?),
            Condition::And(left, right) => Ok(left.holds(resolve)? && right.holds(resolve)?),
            Condition::Not(inner) => Ok(!inner.holds(resolve)?),
            Condition::Holds(term) => match term.value(resolve)? {
                Operand::Boolean(value) => Ok(value),
                other => Err(format!("{other:?} isn't true or false")),
            },
            Condition::Compare(left, comparison, right) => {
                compare(&left.value(resolve)?, *comparison, &right.value(resolve)?)
            }
        }
    }
}

impl Term {
    fn value(
        &self,
        resolve: &mut dyn FnMut(&str) -> Result<Operand, String>,
    ) -> Result<Operand, String> {
        match self {
            Term::Literal(operand) => Ok(operand.clone()),
            Term::Path(path) => resolve(path),
        }
    }
}

fn compare(left: &Operand, comparison: Comparison, right: &Operand) -> Result<bool, String> {
    use std::cmp::Ordering;
    let ordering: Option<Ordering> = match (left, right) {
        (Operand::Integer(left), Operand::Integer(right)) => Some(left.cmp(right)),
        (Operand::Integer(_) | Operand::Decimal(_), Operand::Integer(_) | Operand::Decimal(_)) => {
            number(left).partial_cmp(&number(right))
        }
        (Operand::Text(left), Operand::Text(right)) => Some(left.cmp(right)),
        (Operand::Char(left), Operand::Char(right)) => Some(left.cmp(right)),
        (Operand::Char(char), Operand::Text(text)) => Some(char.to_string().cmp(text)),
        (Operand::Text(text), Operand::Char(char)) => Some(text.cmp(&char.to_string())),
        (Operand::Boolean(left), Operand::Boolean(right)) => Some(left.cmp(right)),
        _ => None,
    };
    let is_equality = matches!(comparison, Comparison::Equal | Comparison::NotEqual);
    let equal = match ordering {
        Some(ordering) => ordering == Ordering::Equal,
        None if is_equality => left == right,
        None => return Err(format!("can't order {left:?} and {right:?}")),
    };
    let ordering = ordering.unwrap_or(Ordering::Equal);
    Ok(match comparison {
        Comparison::Equal => equal,
        Comparison::NotEqual => !equal,
        Comparison::Less => ordering == Ordering::Less,
        Comparison::LessOrEqual => ordering != Ordering::Greater,
        Comparison::Greater => ordering == Ordering::Greater,
        Comparison::GreaterOrEqual => ordering != Ordering::Less,
    })
}

fn number(operand: &Operand) -> f64 {
    match operand {
        Operand::Integer(value) => *value as f64,
        Operand::Decimal(value) => *value,
        _ => f64::NAN,
    }
}

#[derive(Clone, PartialEq, Debug)]
enum Token {
    Path(String),
    Literal(Operand),
    Compare(Comparison),
    And,
    Or,
    Not,
    Open,
    Close,
}

fn tokens(text: &str) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut position = 0;
    while position < chars.len() {
        let char = chars[position];
        let next = chars.get(position + 1).copied();
        let two: String = [Some(char), next].iter().flatten().collect();
        let operator = match two.as_str() {
            "==" => Some(Token::Compare(Comparison::Equal)),
            "!=" => Some(Token::Compare(Comparison::NotEqual)),
            "<=" => Some(Token::Compare(Comparison::LessOrEqual)),
            ">=" => Some(Token::Compare(Comparison::GreaterOrEqual)),
            "&&" => Some(Token::And),
            "||" => Some(Token::Or),
            _ => None,
        };
        if let Some(operator) = operator {
            tokens.push(operator);
            position += 2;
            continue;
        }
        let single = match char {
            '<' => Some(Token::Compare(Comparison::Less)),
            '>' => Some(Token::Compare(Comparison::Greater)),
            '!' => Some(Token::Not),
            '(' => Some(Token::Open),
            ')' => Some(Token::Close),
            _ => None,
        };
        if let Some(single) = single {
            tokens.push(single);
            position += 1;
            continue;
        }
        if char.is_whitespace() {
            position += 1;
            continue;
        }
        if char == '"' || char == '\'' {
            let (literal, end) = quoted(&chars, position)?;
            tokens.push(Token::Literal(literal));
            position = end;
            continue;
        }
        let start = position;
        let is_number = char.is_ascii_digit()
            || (char == '-' && next.is_some_and(|next| next.is_ascii_digit()));
        if is_number {
            position += 1;
            while position < chars.len()
                && (chars[position].is_ascii_digit()
                    || chars[position] == '.'
                    || chars[position] == '_')
            {
                position += 1;
            }
            // Kotlin's suffixes: `10L`, `1.5f`.
            let digits: String = chars[start..position]
                .iter()
                .filter(|char| **char != '_')
                .collect();
            if position < chars.len()
                && matches!(chars[position], 'L' | 'l' | 'f' | 'F' | 'd' | 'D')
            {
                position += 1;
            }
            let literal = match digits.parse::<i64>() {
                Ok(integer) => Operand::Integer(integer),
                Err(_) => Operand::Decimal(
                    digits
                        .parse()
                        .map_err(|_| format!("`{digits}` isn't a number"))?,
                ),
            };
            tokens.push(Token::Literal(literal));
            continue;
        }
        // A path: names, `.`, `[index]`, backticked names.
        let mut depth = 0;
        let mut in_backticks = false;
        while position < chars.len() {
            let char = chars[position];
            let continues = in_backticks
                || depth > 0
                || char.is_alphanumeric()
                || matches!(char, '_' | '$' | '.' | '`' | '[');
            if !continues {
                break;
            }
            match char {
                '`' => in_backticks = !in_backticks,
                '[' if !in_backticks => depth += 1,
                ']' if !in_backticks => depth -= 1,
                _ => {}
            }
            position += 1;
        }
        if position == start {
            return Err(format!("unexpected `{char}`"));
        }
        let word: String = chars[start..position].iter().collect();
        tokens.push(match word.as_str() {
            "true" => Token::Literal(Operand::Boolean(true)),
            "false" => Token::Literal(Operand::Boolean(false)),
            "null" => Token::Literal(Operand::Null),
            _ => Token::Path(word),
        });
    }
    Ok(tokens)
}

/// A string or char literal starting at `start`, and where it ends.
fn quoted(chars: &[char], start: usize) -> Result<(Operand, usize), String> {
    let quote = chars[start];
    let mut text = String::new();
    let mut position = start + 1;
    while position < chars.len() {
        match chars[position] {
            '\\' => {
                let escaped = chars.get(position + 1).ok_or("an unfinished escape")?;
                text.push(match escaped {
                    'n' => '\n',
                    't' => '\t',
                    other => *other,
                });
                position += 2;
            }
            char if char == quote => {
                let literal = if quote == '\'' {
                    let mut chars = text.chars();
                    match (chars.next(), chars.next()) {
                        (Some(char), None) => Operand::Char(char),
                        _ => return Err(format!("'{text}' isn't one character")),
                    }
                } else {
                    Operand::Text(text)
                };
                return Ok((literal, position + 1));
            }
            char => {
                text.push(char);
                position += 1;
            }
        }
    }
    Err("an unclosed quote".to_owned())
}

struct Parser<'tokens> {
    tokens: &'tokens [Token],
    position: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.position)
    }

    fn or(&mut self) -> Result<Condition, String> {
        let mut condition = self.and()?;
        while self.peek() == Some(&Token::Or) {
            self.position += 1;
            condition = Condition::Or(Box::new(condition), Box::new(self.and()?));
        }
        Ok(condition)
    }

    fn and(&mut self) -> Result<Condition, String> {
        let mut condition = self.not()?;
        while self.peek() == Some(&Token::And) {
            self.position += 1;
            condition = Condition::And(Box::new(condition), Box::new(self.not()?));
        }
        Ok(condition)
    }

    fn not(&mut self) -> Result<Condition, String> {
        if self.peek() == Some(&Token::Not) {
            self.position += 1;
            return Ok(Condition::Not(Box::new(self.not()?)));
        }
        self.comparison()
    }

    fn comparison(&mut self) -> Result<Condition, String> {
        if self.peek() == Some(&Token::Open) {
            self.position += 1;
            let inner = self.or()?;
            if self.peek() != Some(&Token::Close) {
                return Err("a `(` without its `)`".to_owned());
            }
            self.position += 1;
            return Ok(inner);
        }
        let left = self.term()?;
        let Some(Token::Compare(comparison)) = self.peek().cloned() else {
            return Ok(Condition::Holds(left));
        };
        self.position += 1;
        let right = self.term()?;
        Ok(Condition::Compare(left, comparison, right))
    }

    fn term(&mut self) -> Result<Term, String> {
        let term = match self.peek() {
            Some(Token::Literal(literal)) => Term::Literal(literal.clone()),
            Some(Token::Path(path)) => Term::Path(path.clone()),
            Some(other) => return Err(format!("expected a value, found {other:?}")),
            None => return Err("expected a value at the end".to_owned()),
        };
        self.position += 1;
        Ok(term)
    }
}

/// When a breakpoint's hits count: DAP's `hitCondition`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HitCondition {
    /// `5`, `== 5`: the fifth hit only.
    Exactly(u64),
    AtLeast(u64),
    MoreThan(u64),
    LessThan(u64),
    AtMost(u64),
    /// `% 10`: every tenth.
    EveryNth(u64),
}

impl HitCondition {
    pub fn parse(text: &str) -> Result<HitCondition, String> {
        let text = text.trim();
        let (constructor, number): (fn(u64) -> HitCondition, &str) =
            if let Some(rest) = text.strip_prefix(">=") {
                (HitCondition::AtLeast, rest)
            } else if let Some(rest) = text.strip_prefix("<=") {
                (HitCondition::AtMost, rest)
            } else if let Some(rest) = text.strip_prefix("==") {
                (HitCondition::Exactly, rest)
            } else if let Some(rest) = text.strip_prefix('>') {
                (HitCondition::MoreThan, rest)
            } else if let Some(rest) = text.strip_prefix('<') {
                (HitCondition::LessThan, rest)
            } else if let Some(rest) = text.strip_prefix('%') {
                (HitCondition::EveryNth, rest)
            } else {
                (HitCondition::Exactly, text)
            };
        let count = number
            .trim()
            .parse()
            .map_err(|_| format!("`{text}` isn't a hit count like `5`, `>= 5` or `% 5`"))?;
        Ok(constructor(count))
    }

    /// Whether the `hits`-th hit (from 1) stops.
    pub fn is_met(self, hits: u64) -> bool {
        match self {
            HitCondition::Exactly(count) => hits == count,
            HitCondition::AtLeast(count) => hits >= count,
            HitCondition::MoreThan(count) => hits > count,
            HitCondition::LessThan(count) => hits < count,
            HitCondition::AtMost(count) => hits <= count,
            HitCondition::EveryNth(count) => count > 0 && hits.is_multiple_of(count),
        }
    }
}

/// A logpoint's message: text, and paths in braces whose values fill in.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LogMessage {
    pub pieces: Vec<LogPiece>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum LogPiece {
    Text(String),
    Path(String),
}

impl LogMessage {
    /// `sum is {sum}`; `{{` and `}}` are braces.
    pub fn parse(text: &str) -> Result<LogMessage, String> {
        let mut pieces = Vec::new();
        let mut literal = String::new();
        let mut chars = text.chars().peekable();
        while let Some(char) = chars.next() {
            match char {
                '{' if chars.peek() == Some(&'{') => {
                    chars.next();
                    literal.push('{');
                }
                '}' if chars.peek() == Some(&'}') => {
                    chars.next();
                    literal.push('}');
                }
                '{' => {
                    let path: String = chars.by_ref().take_while(|char| *char != '}').collect();
                    if path.trim().is_empty() {
                        return Err(format!("an empty `{{}}` in `{text}`"));
                    }
                    if !literal.is_empty() {
                        pieces.push(LogPiece::Text(std::mem::take(&mut literal)));
                    }
                    pieces.push(LogPiece::Path(path.trim().to_owned()));
                }
                char => literal.push(char),
            }
        }
        if !literal.is_empty() {
            pieces.push(LogPiece::Text(literal));
        }
        Ok(LogMessage { pieces })
    }

    /// The message, each path's value from `show` (or why not).
    pub fn render(&self, show: &mut dyn FnMut(&str) -> Result<String, String>) -> String {
        self.pieces
            .iter()
            .map(|piece| match piece {
                LogPiece::Text(text) => text.clone(),
                LogPiece::Path(path) => show(path).unwrap_or_else(|why| format!("<{why}>")),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn holds(condition: &str, values: &[(&str, Operand)]) -> Result<bool, String> {
        let condition = Condition::parse(condition)?;
        condition.holds(&mut |path| {
            values
                .iter()
                .find(|(name, _)| *name == path)
                .map(|(_, value)| value.clone())
                .ok_or_else(|| format!("no `{path}`"))
        })
    }

    #[test]
    fn conditions_compare_paths_and_literals() {
        let values = [
            ("count", Operand::Integer(4)),
            ("order.id", Operand::Text("A-1".to_owned())),
            ("price", Operand::Decimal(1.5)),
            ("items[0].active", Operand::Boolean(true)),
            ("customer", Operand::Null),
            ("grade", Operand::Char('B')),
        ];
        let cases = [
            ("count > 3", true),
            ("count >= 5", false),
            ("count == 4 && order.id == \"A-1\"", true),
            ("count < 4 || price <= 1.5", true),
            ("!(count != 4)", true),
            ("items[0].active", true),
            ("!items[0].active || customer != null", false),
            ("customer == null", true),
            ("price > 1", true),
            ("grade == 'B'", true),
            ("count == 4L", true),
        ];
        for (condition, expected) in cases {
            assert_eq!(holds(condition, &values), Ok(expected), "{condition}");
        }
        assert!(holds("count", &values).is_err(), "a number isn't true");
        assert!(holds("order.id < 3", &values).is_err(), "text and a number");
        assert!(holds("missing == 1", &values).is_err());
        assert!(Condition::parse("count >").is_err());
        assert!(Condition::parse("(count > 1").is_err());
        assert!(Condition::parse("count > 1 1").is_err());
    }

    #[test]
    fn hit_counts_are_exact_bounded_or_periodic() {
        let fifth = HitCondition::parse("5").unwrap();
        assert!(!fifth.is_met(4) && fifth.is_met(5) && !fifth.is_met(6));
        assert!(HitCondition::parse(">= 3").unwrap().is_met(7));
        assert!(!HitCondition::parse("> 3").unwrap().is_met(3));
        assert!(HitCondition::parse("<=2").unwrap().is_met(2));
        let every_third = HitCondition::parse("% 3").unwrap();
        assert!(every_third.is_met(6) && !every_third.is_met(7));
        assert!(HitCondition::parse("often").is_err());
    }

    #[test]
    fn a_log_message_fills_in_its_paths() {
        let message =
            LogMessage::parse("sum is {sum}, first {items[0]} {{literal}} {missing}").unwrap();
        let rendered = message.render(&mut |path| match path {
            "sum" => Ok("7".to_owned()),
            "items[0]" => Ok("3".to_owned()),
            _ => Err(format!("no `{path}`")),
        });
        assert_eq!(rendered, "sum is 7, first 3 {literal} <no `missing`>");
        assert!(LogMessage::parse("empty {}").is_err());
    }
}
