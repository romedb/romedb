//! Graph statements: edge declarations, RELATE and the GRAPH(...) source body.
use crate::{tokenize, validate_delimiter_depth, Error, Expr, Kind, Parser, Record, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Out,
    In,
    Both,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphMode {
    Reachable,
    Paths,
}
#[derive(Clone, Debug, PartialEq)]
pub struct GraphStep {
    pub table: String,
    pub alias: Option<String>,
    pub predicate: Option<Expr>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct GraphHop {
    pub direction: Direction,
    pub edge: GraphStep,
    pub node: GraphStep,
}
#[derive(Clone, Debug, PartialEq)]
pub struct GraphQuery {
    pub root: Expr,
    pub hops: Vec<GraphHop>,
    pub depth: Option<(u32, u32)>,
    pub mode: Option<GraphMode>,
    pub with_path: bool,
}

/// Parse the text between the parentheses of a `GRAPH(...)` source.
pub fn parse_graph(input: &str) -> Result<GraphQuery> {
    let tokens = tokenize(input)?;
    validate_delimiter_depth(&tokens)?;
    let mut p = Parser {
        input,
        tokens,
        pos: 0,
    };
    let root = p.endpoint()?;
    let mut hops = Vec::new();
    while let Some(direction) = p.arrow()? {
        let edge = p.step()?;
        if p.arrow()? != Some(direction) {
            return Err(p.error("a graph hop uses the same arrow on both sides of its edge"));
        }
        let node = p.step()?;
        hops.push(GraphHop {
            direction,
            edge,
            node,
        });
    }
    if hops.is_empty() {
        return Err(p.error("expected an arrow after the graph root"));
    }
    let mut depth = None;
    let mut mode = None;
    let mut with_path = false;
    while p.pos < p.tokens.len() {
        if p.eat("DEPTH") {
            if depth.is_some() {
                return Err(p.error("duplicate DEPTH"));
            }
            let min = p.depth_bound()?;
            if !p.eat("TO") {
                return Err(p.error("expected DEPTH minimum TO maximum"));
            }
            let max = p.depth_bound()?;
            if min > max {
                return Err(p.error("DEPTH minimum exceeds maximum"));
            }
            depth = Some((min, max));
        } else if p.eat("MODE") {
            if mode.is_some() {
                return Err(p.error("duplicate MODE"));
            }
            mode = Some(if p.eat("REACHABLE") {
                GraphMode::Reachable
            } else if p.eat("PATHS") {
                GraphMode::Paths
            } else {
                return Err(p.error("expected MODE REACHABLE or MODE PATHS"));
            });
        } else if p.eat("WITH") {
            if with_path || !p.eat("PATH") {
                return Err(p.error("expected one WITH PATH"));
            }
            with_path = true;
        } else {
            return Err(p.error("unexpected graph clause"));
        }
    }
    if depth.is_some() != mode.is_some() {
        return Err(Error {
            offset: input.len(),
            message: "recursive graph queries require both DEPTH and MODE".into(),
        });
    }
    if depth.is_some() && hops.len() != 1 {
        return Err(Error {
            offset: input.len(),
            message: "recursive graph queries repeat exactly one edge and node step".into(),
        });
    }
    Ok(GraphQuery {
        root,
        hops,
        depth,
        mode,
        with_path: with_path || mode == Some(GraphMode::Paths),
    })
}

impl Parser<'_> {
    /// A fixed record, `type::record(...)` call or parameter.
    pub(crate) fn endpoint(&mut self) -> Result<Expr> {
        let start = self.pos;
        let expr = self.atom(0)?;
        match &expr {
            Expr::Record(_) | Expr::Parameter(_) => Ok(expr),
            Expr::Call(name, _) if name.eq_ignore_ascii_case("type::record") => Ok(expr),
            _ => {
                self.pos = start;
                Err(self.error("graph endpoints use a record ID, type::record(...) or a parameter"))
            }
        }
    }
    pub(crate) fn arrow(&mut self) -> Result<Option<Direction>> {
        let adjacent = |p: &Self, offset: usize, text: &str| {
            p.tokens.get(p.pos + offset).is_some_and(|t| {
                t.kind == Kind::Symbol
                    && t.text == text
                    && (offset == 0 || p.tokens[p.pos + offset - 1].end == t.start)
            })
        };
        if adjacent(self, 0, "<") && adjacent(self, 1, "-") {
            if adjacent(self, 2, ">") {
                self.pos += 3;
                return Ok(Some(Direction::Both));
            }
            self.pos += 2;
            return Ok(Some(Direction::In));
        }
        if adjacent(self, 0, "-") && adjacent(self, 1, ">") {
            self.pos += 2;
            return Ok(Some(Direction::Out));
        }
        Ok(None)
    }
    fn step(&mut self) -> Result<GraphStep> {
        if !self.eat("(") {
            return Ok(GraphStep {
                table: self.name()?.to_ascii_lowercase(),
                alias: None,
                predicate: None,
            });
        }
        let table = self.name()?.to_ascii_lowercase();
        let alias = if self.eat("AS") {
            Some(self.name()?)
        } else {
            None
        };
        let predicate = if self.eat("WHERE") {
            Some(self.expr(0)?)
        } else {
            None
        };
        if !self.eat(")") {
            return Err(self.error("expected ) after graph step"));
        }
        Ok(GraphStep {
            table,
            alias,
            predicate,
        })
    }
    fn depth_bound(&mut self) -> Result<u32> {
        let token = self
            .tokens
            .get(self.pos)
            .ok_or_else(|| self.error("expected depth bound"))?;
        if token.kind != Kind::Number {
            return Err(self.error("depth bounds are nonnegative integers"));
        }
        let value = token
            .text
            .parse()
            .map_err(|_| self.error("depth bounds are nonnegative integers"))?;
        self.pos += 1;
        Ok(value)
    }
    /// `edge` or `edge:key` between RELATE arrows.
    pub(crate) fn relate_edge(&mut self) -> Result<(String, Option<Record>)> {
        if self.tokens.get(self.pos + 1).is_some_and(|t| t.text == ":")
            && self.tokens.get(self.pos + 2).is_none_or(|t| t.text != ":")
        {
            let record = self.record()?;
            return Ok((record.table.clone(), Some(record)));
        }
        Ok((self.name()?.to_ascii_lowercase(), None))
    }
}
