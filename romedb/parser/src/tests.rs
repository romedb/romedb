use super::*;
#[test]
fn content_and_merge_keep_targets_parameters_and_returning() {
    let Statement::MutateDocument {
        table,
        target,
        mode,
        value,
        predicate,
        returning,
    } = parse("UPDATE docs CONTENT $body WHERE id=$id RETURNING id;").unwrap()
    else {
        panic!()
    };
    assert_eq!(table, "docs");
    assert_eq!(target, None);
    assert_eq!(mode, MutationMode::Content);
    assert_eq!(value, Expr::Parameter("$body".into()));
    assert_eq!(predicate, Some("id=$id".into()));
    assert_eq!(returning, Some("id".into()));
    assert!(matches!(
        parse("UPDATE docs:one MERGE {nested:{n:1}} RETURNING *").unwrap(),
        Statement::MutateDocument {
            mode: MutationMode::Merge,
            target: Some(_),
            ..
        }
    ));
    assert!(
        matches!(parse("UPDATE docs {n:1} WHERE title='RETURNING' RETURNING n").unwrap(),Statement::PatchWhere{predicate:Some(p),..} if p=="title='RETURNING'")
    );
    assert!(matches!(
        parse("UPDATE docs:one PATCH [{op:'remove',path:'/n'}] RETURNING id").unwrap(),
        Statement::MutateDocument {
            mode: MutationMode::Patch,
            ..
        }
    ));
    for sql in [
        "UPDATE docs:one PATCH [] WHERE true",
        "UPDATE docs:one CONTENT {} WHERE true",
        "UPDATE docs CONTENT",
        "UPDATE docs MERGE {} WHERE",
        "UPDATE docs CONTENT {} RETURNING",
        "UPDATE docs CONTENT {}; DELETE FROM docs",
    ] {
        assert!(parse(sql).is_err(), "{sql}");
    }
    for sql in [
        "UPDATE content SET merge=1",
        "UPDATE docs AS content SET merge=2",
        "UPDATE docs SET content=$content RETURNING merge",
    ] {
        assert_eq!(parse(sql).unwrap(), Statement::Sql(sql.into()));
    }
}
#[test]
fn sql_is_preserved() {
    for sql in [
        "CREATE TABLE posts (id INTEGER PRIMARY KEY);",
        "CREATE TABLE posts AS SELECT 1;",
        "CREATE TABLE posts ();",
        "SELECT r'posts:p1'",
        "SELECT :name, @name, $name::suffix, ?1, ?",
        "SELECT [posts:p1], 'users:u1'",
        "SELECT 1 AS document",
        "SELECT 'a; b' /* ; */;",
    ] {
        assert_eq!(
            parse(sql).expect("SQL dispatch"),
            Statement::Sql(sql.into())
        );
    }
}

#[test]
fn patch_arrays_do_not_change_sql_bracket_names() {
    for sql in [
        "UPDATE [docs] AS [patch] SET [items] = [other]",
        "SELECT [patch], [a[0] FROM [docs]",
        "SELECT [items] [PATCH] FROM [docs]",
    ] {
        assert_eq!(parse(sql).unwrap(), Statement::Sql(sql.into()));
    }
    let Statement::MutateDocument {
        value,
        predicate,
        returning,
        ..
    } = parse(
        "UPDATE docs PATCH [{op:'add',path:'/bracket]',value:[1,[2]]}] WHERE [n]=1 RETURNING [id]",
    )
    .unwrap()
    else {
        panic!()
    };
    assert!(matches!(value, Expr::Array(_)));
    assert_eq!(predicate, Some("[n]=1".into()));
    assert_eq!(returning, Some("[id]".into()));
}
#[test]
fn collection_and_records() {
    assert_eq!(
        parse("CREATE TABLE IF NOT EXISTS posts; -- hi").expect("collection"),
        Statement::CreateCollection {
            name: "posts".into(),
            if_not_exists: true
        }
    );
    assert_eq!(
        parse("SELECT posts:`123`;").expect("record"),
        Statement::SelectRecord(Record {
            table: "posts".into(),
            key: Key::String("123".into())
        })
    );
    assert_eq!(
        parse("SELECT posts:-9223372036854775808").expect("record"),
        Statement::SelectRecord(Record {
            table: "posts".into(),
            key: Key::Integer(i64::MIN)
        })
    );
    assert!(parse("SELECT posts:9223372036854775808").is_err());
}
#[test]
fn document_nested_arrays_and_duplicate_keys() {
    let Statement::Insert { value: Expr::Object(fields), .. } = parse("INSERT INTO posts {id: posts:p1, tags: [1, [2, 3], {name: 'a]b'}], profile: {city: 'Bangkok'},} RETURNING *;").expect("nested literal") else { panic!("expected insert"); };
    assert!(matches!(fields["tags"], Expr::Array(_)));
    assert!(parse("INSERT INTO posts {a: 1, a: 2}").is_err());
    assert!(parse("INSERT INTO posts {id: posts:``}").is_err());
}

#[test]
fn interactive_script_completeness_tracks_lexical_and_trigger_boundaries() {
    for sql in [
        "",
        "-- comment\n",
        "SELECT ';';",
        "SELECT 1; /* done */",
        "CREATE TRIGGER t AFTER INSERT ON s BEGIN SELECT CASE WHEN 1 THEN 2 END; END;",
    ] {
        assert!(crate::script_complete(sql).unwrap(), "{sql}");
    }
    for sql in [
        "SELECT 1",
        "SELECT 'unfinished",
        "SELECT 1; /* open",
        "INSERT INTO docs {v:1",
        "CREATE TRIGGER t AFTER INSERT ON s BEGIN SELECT 1;",
    ] {
        assert!(!crate::script_complete(sql).unwrap(), "{sql}");
    }
    assert!(crate::script_complete("SELECT );").is_err());
}

#[test]
fn delegated_sql_has_a_delimiter_depth_limit_before_engine_parsing() {
    let sql = |depth: usize| format!("SELECT {}1{}", "(".repeat(depth), ")".repeat(depth));
    assert!(parse(&sql(64)).is_ok());
    let error = parse(&sql(65)).unwrap_err();
    assert!(error.message.contains("delimiter nesting limit"));
    assert_eq!(error.offset, "SELECT ".len() + 64);
    assert!(parse(&sql(10_000)).is_err());
    for quoted in [
        format!("'{}'", "(".repeat(100)),
        format!("\"{}\"", "(".repeat(100)),
        format!("[{}]", "(".repeat(100)),
    ] {
        assert!(parse(&format!("SELECT {quoted} /* {} */", "(".repeat(100))).is_ok());
    }
}

#[test]
fn tokenizer_bounds_bytes_before_scanning_and_tokens_before_copying() {
    let exact = " ".repeat(MAX_INPUT_BYTES);
    assert!(tokenize(&exact).unwrap().is_empty());
    let error = tokenize(&(exact + "é")).unwrap_err();
    assert_eq!(error.offset, 0);
    assert!(error.message.contains("input byte limit"));
    let exact = "x ".repeat(MAX_TOKENS);
    assert_eq!(
        tokenize(&(exact.clone() + "/* ignored */")).unwrap().len(),
        MAX_TOKENS
    );
    let error = tokenize(&(exact.clone() + "'next'")).unwrap_err();
    assert_eq!(error.offset, exact.len());
    assert!(error.message.contains("input token limit"));
    assert!(split_script(&(exact + "x"))
        .unwrap_err()
        .message
        .contains("input token limit"));
    assert!(tokenize("SELECT 'x x x', /* x x */ 1").is_ok());
}

#[test]
fn spatial_index_declarations_are_additive_and_strict() {
    assert_eq!(parse("CREATE SEARCH INDEX IF NOT EXISTS locations ON places(profile.location) USING SPATIAL;").unwrap(),
        Statement::CreateSpatialIndex { if_not_exists:true, table:"places".into(), name:"locations".into(), path:vec!["profile".into(),"location".into()], regions:false });
    for sql in [
        "CREATE SEARCH INDEX i ON places(a,b) USING SPATIAL",
        "CREATE SEARCH INDEX i ON places(a) USING unknown",
        "CREATE SEARCH INDEX i ON places(a) USING SPATIAL extra",
        "CREATE SEARCH INDEX i ON places(a) USING SPATIAL WITH (geometry = 'line')",
        "CREATE SEARCH INDEX i ON places(a) USING SPATIAL WITH (shape = 'region')",
        "CREATE SEARCH INDEX i ON places(a) USING SPATIAL WITH (geometry = region)",
        "CREATE SEARCH INDEX i ON places(a) USING SPATIAL WITH (geometry = 'region') extra",
    ] {
        assert!(parse(sql).is_err(), "{sql}");
    }
    assert_eq!(parse("CREATE SEARCH INDEX zones_shape ON zones(shape) USING SPATIAL WITH (geometry = 'region')").unwrap(),
        Statement::CreateSpatialIndex { if_not_exists:false, table:"zones".into(), name:"zones_shape".into(), path:vec!["shape".into()], regions:true });
    assert_eq!(
        parse("CREATE SEARCH INDEX p ON places(location) USING SPATIAL WITH (GEOMETRY = 'point')")
            .unwrap(),
        Statement::CreateSpatialIndex {
            if_not_exists: false,
            table: "places".into(),
            name: "p".into(),
            path: vec!["location".into()],
            regions: false
        }
    );
    assert!(matches!(
        parse("CREATE INDEX i ON places(a)").unwrap(),
        Statement::CreateIndex { .. }
    ));
}

#[test]
fn fulltext_index_declarations_preserve_ordered_paths() {
    assert_eq!(
        parse(
            "CREATE SEARCH INDEX IF NOT EXISTS titles ON posts(title,profile.body) USING FULLTEXT;"
        )
        .unwrap(),
        Statement::CreateFullTextIndex {
            if_not_exists: true,
            table: "posts".into(),
            name: "titles".into(),
            paths: vec![vec!["title".into()], vec!["profile".into(), "body".into()]],
            tokenizer: None,
            min_gram: None,
            max_gram: None
        }
    );
    for sql in [
        "CREATE SEARCH INDEX i ON posts() USING FULLTEXT",
        "CREATE SEARCH INDEX i ON posts(title,) USING FULLTEXT",
        "CREATE SEARCH INDEX i ON posts(title) USING FULLTEXT extra",
        "CREATE SEARCH INDEX i ON posts(title + body) USING FULLTEXT",
    ] {
        assert!(parse(sql).is_err(), "{sql}");
    }
}

#[test]
fn record_brace_projection_grammar_preserves_plain_record_expressions() {
    let Statement::SelectRecordProjection { target, fields } =
        parse("SELECT posts:p1 {title,author.*,profile.city AS city,*,}").unwrap()
    else {
        panic!()
    };
    assert_eq!(target.table, "posts");
    assert_eq!(fields.len(), 4);
    assert_eq!(fields[1].path, vec!["author"]);
    assert!(fields[1].expand);
    assert_eq!(fields[2].alias.as_deref(), Some("city"));
    assert!(fields[3].path.is_empty());
    assert!(matches!(
        parse("SELECT posts:p1 AS ref").unwrap(),
        Statement::Sql(_)
    ));
    for sql in [
        "SELECT posts:p1 {}",
        "SELECT posts:p1 {author {name}}",
        "SELECT posts:p1 {author.*.name}",
        "SELECT posts:p1 {title} WHERE 1",
    ] {
        assert!(parse(sql).is_err(), "{sql}");
    }
}

#[test]
fn declared_inverse_relationship_grammar() {
    assert!(
        matches!(parse("DEFINE RELATION authored ON users FROM posts.meta.author").unwrap(),
        Statement::DefineRelation {path, ..} if path == ["meta", "author"])
    );
    assert!(matches!(
        parse("DROP RELATION IF EXISTS authored").unwrap(),
        Statement::DropRelation {
            if_exists: true,
            ..
        }
    ));
    assert!(
        matches!(parse("INFO FOR RELATION authored").unwrap(), Statement::Info {scope,..} if scope == "relation")
    );
    for sql in [
        "DEFINE RELATION r ON users FROM posts",
        "DEFINE RELATION r ON users FROM posts.a extra",
        "DROP RELATION IF r",
        "DROP RELATION r extra",
    ] {
        assert!(parse(sql).is_err(), "{sql}");
    }
    assert!(matches!(
        parse("CREATE TABLE relation(id TEXT)").unwrap(),
        Statement::Sql(_)
    ));
}

#[test]
fn vector_index_options_are_explicit_unique_and_order_independent() {
    assert!(
        matches!(parse("CREATE SEARCH INDEX compressed ON items(v) USING VECTOR WITH(quantization='f16',metric='cosine',dimensions=3)").unwrap(),Statement::CreateVectorIndex {quantization:Some(value),..} if value=="f16")
    );
    for options in [
        "dimensions=3,metric='cosine'",
        "metric='cosine',dimensions=3",
    ] {
        assert_eq!(parse(&format!("CREATE SEARCH INDEX IF NOT EXISTS embeddings ON items(nested.v) USING VECTOR WITH ({options});")).unwrap(),Statement::CreateVectorIndex {if_not_exists:true,table:"items".into(),name:"embeddings".into(),path:vec!["nested".into(),"v".into()],dimensions:3,metric:"cosine".into(),quantization:None});
    }
    for options in [
        "dimensions=3",
        "metric='l2'",
        "dimensions=3,metric=l2",
        "dimensions=3,dimensions=3,metric='l2'",
        "dimensions=3.5,metric='l2'",
        "dimensions=-1,metric='l2'",
        "dimensions=3,metric='l2',other=1",
        "dimensions=3,metric='l2',quantization=16",
        "dimensions=3,metric='l2',quantization='f16',quantization='f16'",
    ] {
        assert!(
            parse(&format!(
                "CREATE SEARCH INDEX i ON items(v) USING VECTOR WITH ({options})"
            ))
            .is_err(),
            "{options}"
        );
    }
    assert!(parse(
        "CREATE SEARCH INDEX i ON items(v,w) USING VECTOR WITH (dimensions=3,metric='l2')"
    )
    .is_err());
}

#[test]
fn compound_and_array_index_grammar_preserves_native_fallbacks() {
    assert!(
        matches!(parse("CREATE UNIQUE INDEX pair ON docs(a,nested.b)").unwrap(), Statement::CreateIndex {unique:true,path,additional_paths,..} if path==["a"] && additional_paths==[vec!["nested".to_owned(),"b".to_owned()]])
    );
    assert!(
        matches!(parse("CREATE SEARCH INDEX IF NOT EXISTS tags ON docs(nested.tags) USING ARRAY").unwrap(),Statement::CreateArrayIndex {if_not_exists:true,path,..} if path==["nested","tags"])
    );
    for sql in [
        "CREATE SEARCH INDEX tags ON docs(a,b) USING ARRAY",
        "CREATE SEARCH INDEX tags ON docs(a) USING ARRAY WITH(max=10)",
    ] {
        assert!(parse(sql).is_err(), "{sql}");
    }
}

#[test]
fn array_index_paths_step_into_arrays_with_star() {
    assert!(
        matches!(parse("CREATE SEARCH INDEX roles ON docs(team.*.role) USING ARRAY").unwrap(),Statement::CreateArrayIndex {path,..} if path==["team","*","role"])
    );
    assert!(
        matches!(parse("CREATE SEARCH INDEX kinds ON docs(team . * . links.*.kind) USING ARRAY").unwrap(),Statement::CreateArrayIndex {path,..} if path==["team","*","links","*","kind"])
    );
    for sql in [
        "CREATE SEARCH INDEX roles ON docs(team.*) USING ARRAY",
        "CREATE SEARCH INDEX roles ON docs(*.role) USING ARRAY",
        "CREATE SEARCH INDEX roles ON docs(team.\"*\".role) USING ARRAY",
        "CREATE SEARCH INDEX roles ON docs(\"*\") USING ARRAY",
        "CREATE SEARCH INDEX roles ON docs(team.*.role) USING FULLTEXT",
        "CREATE SEARCH INDEX roles ON docs(team.*.at) USING SPATIAL",
        "CREATE SEARCH INDEX roles ON docs(team.*.v) USING VECTOR WITH (dimensions=3,metric='l2')",
        "CREATE INDEX roles ON docs(team.*.role)",
        "CREATE UNIQUE INDEX roles ON docs(team.*.role)",
    ] {
        assert!(
            !matches!(
                parse(sql),
                Ok(Statement::CreateArrayIndex { .. }
                    | Statement::CreateIndex { .. }
                    | Statement::CreateFullTextIndex { .. }
                    | Statement::CreateSpatialIndex { .. }
                    | Statement::CreateVectorIndex { .. })
            ),
            "{sql}"
        );
    }
    for sql in [
        "CREATE INDEX native ON relational(a DESC,b COLLATE NOCASE)",
        "CREATE INDEX native ON relational(a) WHERE a>0",
    ] {
        assert!(matches!(parse(sql).unwrap(), Statement::Sql(_)), "{sql}");
    }
}

#[test]
fn javascript_function_ddl_keeps_source_and_typed_signatures() {
    assert_eq!(parse("CREATE OR REPLACE FUNCTION App::normalize(value string, fallback string?) RETURNS string? LANGUAGE JAVASCRIPT AS 'return value || fallback;';").unwrap(),Statement::CreateFunction {name:"app::normalize".into(),parameters:vec![("value".into(),"string".into()),("fallback".into(),"string?".into())],returns:"string?".into(),source:"return value || fallback;".into(),replace:true});
    assert_eq!(
        parse("DROP FUNCTION IF EXISTS app::normalize").unwrap(),
        Statement::DropFunction {
            name: "app::normalize".into(),
            if_exists: true
        }
    );
    assert_eq!(
        parse("INFO FOR FUNCTION app::normalize").unwrap(),
        Statement::Info {
            scope: "function".into(),
            name: Some("app::normalize".into())
        }
    );
    for sql in [
        "CREATE FUNCTION f() RETURNS string LANGUAGE JAVASCRIPT AS 'return 1'",
        "CREATE FUNCTION app::f(a) RETURNS string LANGUAGE JAVASCRIPT AS 'return 1'",
        "CREATE FUNCTION app::f() RETURNS string LANGUAGE PYTHON AS 'return 1'",
        "CREATE FUNCTION app::f() RETURNS string LANGUAGE JAVASCRIPT AS 'return 1' extra",
        "DROP FUNCTION IF app::f",
    ] {
        assert!(parse(sql).is_err(), "{sql}");
    }
}

#[test]
fn standalone_expression_parser_rejects_statements_and_trailing_delimiters() {
    assert!(crate::parse_expression("active=true AND this.n >= $min").is_ok());
    for input in [
        "",
        "this;",
        "this; SELECT 1",
        "this other",
        "SELECT 1",
        "this)",
    ] {
        assert!(crate::parse_expression(input).is_err(), "{input}");
    }
}

#[test]
fn field_default_and_readonly_modifiers_preserve_native_default_sql() {
    let Statement::DefineField {
        default,
        readonly,
        required,
        check,
        ..
    } = parse("DEFINE FIELD n ON docs TYPE integer REQUIRED DEFAULT -7 READONLY CHECK(n<0)")
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(default, Some(Expr::Integer(-7)));
    assert!(readonly && required && check.is_some());
    for sql in [
        "DEFINE FIELD n ON docs TYPE integer READONLY DEFAULT 7",
        "DEFINE FIELD n ON docs TYPE integer DEFAULT 7 DEFAULT 8",
        "DEFINE FIELD n ON docs TYPE integer READONLY READONLY",
    ] {
        assert!(parse(sql).is_err(), "{sql}");
    }
    let sql = "CREATE TABLE native(n INTEGER DEFAULT 7)";
    assert_eq!(parse(sql).unwrap(), Statement::Sql(sql.into()));
}

#[test]
fn strict_schema_and_typed_array_modifiers_are_explicit() {
    assert_eq!(
        parse("DEFINE SCHEMA ON docs STRICT").unwrap(),
        Statement::DefineSchema {
            table: "docs".into(),
            strict: true
        }
    );
    assert_eq!(
        parse("DEFINE SCHEMA ON docs FLEXIBLE").unwrap(),
        Statement::DefineSchema {
            table: "docs".into(),
            strict: false
        }
    );
    for target in [
        "integer?",
        "record<writers>",
        "vector<3>?",
        "object",
        "any",
        "array",
    ] {
        let Statement::DefineField {
            kind,
            target: actual,
            flexible,
            ..
        } = parse(&format!(
            "DEFINE FIELD values ON docs TYPE array<{target}> FLEXIBLE"
        ))
        .unwrap()
        else {
            panic!()
        };
        assert_eq!(kind, "array");
        assert_eq!(actual, Some(target.into()));
        assert!(flexible);
    }
    for sql in [
        "DEFINE SCHEMA docs STRICT",
        "DEFINE SCHEMA ON docs",
        "DEFINE SCHEMA ON docs STRICT extra",
        "DEFINE FIELD n ON docs TYPE array<array<integer>>",
        "DEFINE FIELD n ON docs TYPE array<integer??>",
    ] {
        assert!(parse(sql).is_err(), "{sql}");
    }
}

#[test]
fn stored_value_clauses_preserve_checks_and_native_generated_columns() {
    let Statement::DefineField {
        computed, check, ..
    } = parse("DEFINE FIELD n ON docs TYPE integer VALUE (a + length(')')) CHECK(n>0)").unwrap()
    else {
        panic!()
    };
    assert_eq!(computed, Some("a + length(')')".into()));
    assert_eq!(check, Some("n>0".into()));
    for sql in [
        "DEFINE FIELD n ON docs TYPE integer VALUE a+1",
        "DEFINE FIELD n ON docs TYPE integer VALUE (a+1",
        "DEFINE FIELD n ON docs TYPE integer CHECK(n>0) VALUE (a+1)",
    ] {
        assert!(parse(sql).is_err());
    }
    let sql = "CREATE TABLE native(a INTEGER,b INTEGER GENERATED ALWAYS AS (a+1) STORED)";
    assert_eq!(parse(sql).unwrap(), Statement::Sql(sql.into()));
}

#[test]
fn script_patch_arrays_are_scoped_to_their_statement() {
    let script = "SELECT 1; UPDATE docs:one PATCH [{op:'add',path:'/a]',value:[1,[2]]}];";
    let pieces = split_script(script).unwrap();
    assert_eq!(
        pieces.iter().map(|s| s.sql).collect::<Vec<_>>(),
        [
            "SELECT 1;",
            "UPDATE docs:one PATCH [{op:'add',path:'/a]',value:[1,[2]]}];"
        ]
    );
    assert!(script_complete(script).unwrap());
    assert!(matches!(
        parse(pieces[1].sql).unwrap(),
        Statement::MutateDocument {
            mode: MutationMode::Patch,
            ..
        }
    ));
    let script = "UPDATE docs:one PATCH [{op:'remove',path:'/n'}]; SELECT [a)b] FROM t;";
    assert_eq!(split_script(script).unwrap()[1].sql, "SELECT [a)b] FROM t;");
}

#[test]
fn insert_record_targets_become_the_document_id() {
    let Statement::Insert {
        table,
        value: Expr::Object(fields),
        returning,
    } = parse("INSERT INTO posts:first {\n    foo: 'bar'\n} RETURNING *").expect("target insert")
    else {
        panic!("expected insert");
    };
    assert_eq!(table, "posts");
    assert_eq!(returning.as_deref(), Some("*"));
    assert_eq!(
        fields.get("id"),
        Some(&Expr::Record(Record {
            table: "posts".into(),
            key: Key::String("first".into()),
        }))
    );
    assert_eq!(fields.get("foo"), Some(&Expr::String("bar".into())));
    assert_eq!(
        parse("INSERT INTO posts:7 {title: 'x'}").unwrap(),
        parse("INSERT INTO posts {id: posts:7, title: 'x'}").unwrap()
    );
    assert!(parse("INSERT INTO posts:first {id: posts:first, title: 'x'}").is_err());
    assert!(parse("INSERT INTO posts:first DOCUMENT $doc").is_err());
    assert!(parse("INSERT INTO posts:first").is_err());
}

#[test]
fn edge_tables_and_relate_parse_their_clauses() {
    assert_eq!(
        parse("CREATE EDGE TABLE follows FROM people TO people ON DELETE CASCADE;").unwrap(),
        Statement::CreateEdgeTable {
            name: "follows".into(),
            if_not_exists: false,
            source: "people".into(),
            target: "people".into(),
            cascade: true,
        }
    );
    assert!(matches!(
        parse("CREATE EDGE TABLE IF NOT EXISTS wrote FROM people TO articles").unwrap(),
        Statement::CreateEdgeTable {
            if_not_exists: true,
            cascade: false,
            ..
        }
    ));
    for sql in [
        "CREATE EDGE follows FROM a TO b",
        "CREATE EDGE TABLE follows FROM a",
        "CREATE EDGE TABLE follows FROM a TO b ON DELETE SET NULL",
        "CREATE EDGE TABLE follows FROM a TO b extra",
    ] {
        assert!(parse(sql).is_err(), "{sql}");
    }
    let Statement::Relate {
        source,
        edge,
        id,
        target,
        value,
        returning,
    } = parse("RELATE people:alice->follows:ab -> people:bob { weight: 2 } RETURNING id").unwrap()
    else {
        panic!()
    };
    assert_eq!(
        source,
        Expr::Record(Record {
            table: "people".into(),
            key: Key::String("alice".into())
        })
    );
    assert_eq!(edge, "follows");
    assert_eq!(id.unwrap().key, Key::String("ab".into()));
    assert!(matches!(target, Expr::Record(_)));
    assert!(matches!(value, Some(Expr::Object(_))));
    assert_eq!(returning.as_deref(), Some("id"));
    assert!(matches!(
        parse("RELATE type::record('people', $s) -> follows -> $t DOCUMENT $props").unwrap(),
        Statement::Relate {
            source: Expr::Call(_, _),
            target: Expr::Parameter(_),
            value: Some(Expr::Parameter(_)),
            id: None,
            ..
        }
    ));
    for sql in [
        "RELATE people:a <- follows <- people:b",
        "RELATE people:a -> follows people:b",
        "RELATE 'people:a' -> follows -> people:b",
        "RELATE people:a - > follows -> people:b",
    ] {
        assert!(parse(sql).is_err(), "{sql}");
    }
}

#[test]
fn graph_bodies_parse_hops_steps_and_recursion() {
    let graph =
        parse_graph("people:alice -> follows -> people -> wrote -> articles WITH PATH").unwrap();
    assert_eq!(graph.hops.len(), 2);
    assert!(graph.with_path);
    assert_eq!(graph.hops[1].edge.table, "wrote");
    assert_eq!(graph.hops[1].node.table, "articles");
    assert_eq!(
        parse_graph("people:bob <- follows <- people").unwrap().hops[0].direction,
        Direction::In
    );
    assert_eq!(
        parse_graph("people:bob <-> follows <-> people")
            .unwrap()
            .hops[0]
            .direction,
        Direction::Both
    );
    let graph = parse_graph(
        "$root -> (follows AS e WHERE e.weight >= $min) -> (People AS n WHERE n.active = true) DEPTH 0 TO 4 MODE PATHS",
    )
    .unwrap();
    assert_eq!(graph.depth, Some((0, 4)));
    assert_eq!(graph.mode, Some(GraphMode::Paths));
    assert!(graph.with_path);
    assert_eq!(graph.hops[0].edge.alias.as_deref(), Some("e"));
    assert_eq!(graph.hops[0].node.table, "people");
    assert!(graph.hops[0].edge.predicate.is_some());
    for body in [
        "people:a",
        "people:a -> follows <- people",
        "people:a -> follows -> people DEPTH 1 TO 3",
        "people:a -> follows -> people MODE REACHABLE",
        "people:a -> follows -> people DEPTH 3 TO 1 MODE REACHABLE",
        "people:a -> follows -> people DEPTH -1 TO 1 MODE REACHABLE",
        "people:a -> f -> p -> g -> q DEPTH 1 TO 2 MODE REACHABLE",
        "people.a -> follows -> people",
        "people:a -> follows -> people LIMIT 3",
        "people:a -> (follows WHERE) -> people",
    ] {
        assert!(parse_graph(body).is_err(), "{body}");
    }
}
