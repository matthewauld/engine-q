use nu_path::canonicalize;
use nu_protocol::{
    ast::{
        Block, Call, Expr, Expression, ImportPattern, ImportPatternHead, ImportPatternMember,
        Pipeline, Statement,
    },
    engine::StateWorkingSet,
    span, Exportable, Overlay, Span, SyntaxShape, Type, CONFIG_VARIABLE_ID,
};
use std::collections::{HashMap, HashSet};

use crate::{
    lex, lite_parse,
    parser::{
        check_name, garbage, garbage_statement, parse, parse_block_expression,
        parse_import_pattern, parse_internal_call, parse_multispan_value, parse_signature,
        parse_string, parse_var_with_opt_type, trim_quotes,
    },
    ParseError,
};

pub fn parse_def_predecl(working_set: &mut StateWorkingSet, spans: &[Span]) -> Option<ParseError> {
    let name = working_set.get_span_contents(spans[0]);

    // handle "export def" same as "def"
    let (name, spans) = if name == b"export" && spans.len() >= 2 {
        (working_set.get_span_contents(spans[1]), &spans[1..])
    } else {
        (name, spans)
    };

    if name == b"def" && spans.len() >= 4 {
        let (name_expr, ..) = parse_string(working_set, spans[1]);
        let name = name_expr.as_string();

        working_set.enter_scope();
        // FIXME: because parse_signature will update the scope with the variables it sees
        // we end up parsing the signature twice per def. The first time is during the predecl
        // so that we can see the types that are part of the signature, which we need for parsing.
        // The second time is when we actually parse the body itworking_set.
        // We can't reuse the first time because the variables that are created during parse_signature
        // are lost when we exit the scope below.
        let (sig, ..) = parse_signature(working_set, spans[2]);
        let signature = sig.as_signature();
        working_set.exit_scope();

        if let (Some(name), Some(mut signature)) = (name, signature) {
            signature.name = name;
            let decl = signature.predeclare();

            if working_set.add_predecl(decl).is_some() {
                return Some(ParseError::DuplicateCommandDef(spans[1]));
            }
        }
    }

    None
}

pub fn parse_def(
    working_set: &mut StateWorkingSet,
    spans: &[Span],
) -> (Statement, Option<ParseError>) {
    let mut error = None;
    let name = working_set.get_span_contents(spans[0]);

    if name == b"def" {
        // TODO: Convert all 'expect("internal error: ...")' to ParseError::InternalError
        let def_decl_id = working_set
            .find_decl(b"def")
            .expect("internal error: missing def command");

        let mut call = Box::new(Call {
            head: spans[0],
            decl_id: def_decl_id,
            positional: vec![],
            named: vec![],
        });

        if let Some(name_span) = spans.get(1) {
            let (name_expr, err) = parse_string(working_set, *name_span);
            error = error.or(err);

            let name = name_expr.as_string();
            call.positional.push(name_expr);

            if let Some(sig_span) = spans.get(2) {
                working_set.enter_scope();
                let (sig, err) = parse_signature(working_set, *sig_span);
                error = error.or(err);

                let signature = sig.as_signature();

                call.positional.push(sig);

                if let Some(block_span) = spans.get(3) {
                    let (block, err) = parse_block_expression(
                        working_set,
                        &SyntaxShape::Block(Some(vec![])),
                        *block_span,
                    );
                    error = error.or(err);

                    let block_id = block.as_block();

                    call.positional.push(block);

                    if let (Some(name), Some(mut signature), Some(block_id)) =
                        (&name, signature, block_id)
                    {
                        if let Some(decl_id) = working_set.find_decl(name.as_bytes()) {
                            let declaration = working_set.get_decl_mut(decl_id);

                            signature.name = name.clone();

                            *declaration = signature.into_block_command(block_id);
                        } else {
                            error = error.or_else(|| {
                                Some(ParseError::InternalError(
                                    "Predeclaration failed to add declaration".into(),
                                    spans[1],
                                ))
                            });
                        };
                    }
                } else {
                    let err_span = Span {
                        start: sig_span.end,
                        end: sig_span.end,
                    };

                    error = error
                        .or_else(|| Some(ParseError::MissingPositional("block".into(), err_span)));
                }
                working_set.exit_scope();

                if let Some(name) = name {
                    // It's OK if it returns None: The decl was already merged in previous parse
                    // pass.
                    working_set.merge_predecl(name.as_bytes());
                } else {
                    error = error.or_else(|| {
                        Some(ParseError::UnknownState(
                            "Could not get string from string expression".into(),
                            *name_span,
                        ))
                    });
                }
            } else {
                let err_span = Span {
                    start: name_span.end,
                    end: name_span.end,
                };

                error = error
                    .or_else(|| Some(ParseError::MissingPositional("parameters".into(), err_span)));
            }
        } else {
            let err_span = Span {
                start: spans[0].end,
                end: spans[0].end,
            };

            error = error.or_else(|| {
                Some(ParseError::MissingPositional(
                    "definition name".into(),
                    err_span,
                ))
            });
        }

        (
            Statement::Pipeline(Pipeline::from_vec(vec![Expression {
                expr: Expr::Call(call),
                span: span(spans),
                ty: Type::Unknown,
                custom_completion: None,
            }])),
            error,
        )
    } else {
        (
            garbage_statement(spans),
            Some(ParseError::UnknownState(
                "Expected structure: def <name> [] {}".into(),
                span(spans),
            )),
        )
    }
}

pub fn parse_alias(
    working_set: &mut StateWorkingSet,
    spans: &[Span],
) -> (Statement, Option<ParseError>) {
    let name = working_set.get_span_contents(spans[0]);

    if name == b"alias" {
        if let Some((span, err)) = check_name(working_set, spans) {
            return (
                Statement::Pipeline(Pipeline::from_vec(vec![garbage(*span)])),
                Some(err),
            );
        }

        if let Some(decl_id) = working_set.find_decl(b"alias") {
            let (call, _) = parse_internal_call(working_set, spans[0], &spans[1..], decl_id);

            if spans.len() >= 4 {
                let alias_name = working_set.get_span_contents(spans[1]);

                let alias_name = if alias_name.starts_with(b"\"")
                    && alias_name.ends_with(b"\"")
                    && alias_name.len() > 1
                {
                    alias_name[1..(alias_name.len() - 1)].to_vec()
                } else {
                    alias_name.to_vec()
                };
                let _equals = working_set.get_span_contents(spans[2]);

                let replacement = spans[3..].to_vec();

                working_set.add_alias(alias_name, replacement);
            }

            return (
                Statement::Pipeline(Pipeline::from_vec(vec![Expression {
                    expr: Expr::Call(call),
                    span: span(spans),
                    ty: Type::Unknown,
                    custom_completion: None,
                }])),
                None,
            );
        }
    }

    (
        garbage_statement(spans),
        Some(ParseError::InternalError(
            "Alias statement unparseable".into(),
            span(spans),
        )),
    )
}

pub fn parse_export(
    working_set: &mut StateWorkingSet,
    spans: &[Span],
) -> (Statement, Option<Exportable>, Option<ParseError>) {
    let mut error = None;

    let export_span = if let Some(sp) = spans.get(0) {
        if working_set.get_span_contents(*sp) != b"export" {
            return (
                garbage_statement(spans),
                None,
                Some(ParseError::UnknownState(
                    "expected export statement".into(),
                    span(spans),
                )),
            );
        }

        *sp
    } else {
        return (
            garbage_statement(spans),
            None,
            Some(ParseError::UnknownState(
                "got empty input for parsing export statement".into(),
                span(spans),
            )),
        );
    };

    let export_decl_id = if let Some(id) = working_set.find_decl(b"export") {
        id
    } else {
        return (
            garbage_statement(spans),
            None,
            Some(ParseError::InternalError(
                "missing export command".into(),
                export_span,
            )),
        );
    };

    let mut call = Box::new(Call {
        head: spans[0],
        decl_id: export_decl_id,
        positional: vec![],
        named: vec![],
    });

    let exportable = if let Some(kw_span) = spans.get(1) {
        let kw_name = working_set.get_span_contents(*kw_span);
        match kw_name {
            b"def" => {
                let (stmt, err) = parse_def(working_set, &spans[1..]);
                error = error.or(err);

                let export_def_decl_id = if let Some(id) = working_set.find_decl(b"export def") {
                    id
                } else {
                    return (
                        garbage_statement(spans),
                        None,
                        Some(ParseError::InternalError(
                            "missing 'export def' command".into(),
                            export_span,
                        )),
                    );
                };

                // Trying to warp the 'def' call into the 'export def' in a very clumsy way
                if let Statement::Pipeline(ref pipe) = stmt {
                    if let Some(Expression {
                        expr: Expr::Call(ref def_call),
                        ..
                    }) = pipe.expressions.get(0)
                    {
                        call = def_call.clone();

                        call.head = span(&spans[0..=1]);
                        call.decl_id = export_def_decl_id;
                    } else {
                        error = error.or_else(|| {
                            Some(ParseError::InternalError(
                                "unexpected output from parsing a definition".into(),
                                span(&spans[1..]),
                            ))
                        });
                    }
                } else {
                    error = error.or_else(|| {
                        Some(ParseError::InternalError(
                            "unexpected output from parsing a definition".into(),
                            span(&spans[1..]),
                        ))
                    });
                };

                if error.is_none() {
                    let decl_name = working_set.get_span_contents(spans[2]);
                    let decl_name = trim_quotes(decl_name);
                    if let Some(decl_id) = working_set.find_decl(decl_name) {
                        Some(Exportable::Decl(decl_id))
                    } else {
                        error = error.or_else(|| {
                            Some(ParseError::InternalError(
                                "failed to find added declaration".into(),
                                span(&spans[1..]),
                            ))
                        });
                        None
                    }
                } else {
                    None
                }
            }
            b"env" => {
                if let Some(id) = working_set.find_decl(b"export env") {
                    call.decl_id = id;
                } else {
                    return (
                        garbage_statement(spans),
                        None,
                        Some(ParseError::InternalError(
                            "missing 'export env' command".into(),
                            export_span,
                        )),
                    );
                }

                call.head = span(&spans[0..=1]);

                if let Some(name_span) = spans.get(2) {
                    let (name_expr, err) = parse_string(working_set, *name_span);
                    error = error.or(err);
                    call.positional.push(name_expr);

                    if let Some(block_span) = spans.get(3) {
                        let (block_expr, err) = parse_block_expression(
                            working_set,
                            &SyntaxShape::Block(None),
                            *block_span,
                        );
                        error = error.or(err);

                        let exportable = if let Expression {
                            expr: Expr::Block(block_id),
                            ..
                        } = block_expr
                        {
                            Some(Exportable::EnvVar(block_id))
                        } else {
                            error = error.or_else(|| {
                                Some(ParseError::InternalError(
                                    "block was not parsed as a block".into(),
                                    *block_span,
                                ))
                            });
                            None
                        };

                        call.positional.push(block_expr);

                        exportable
                    } else {
                        let err_span = Span {
                            start: name_span.end,
                            end: name_span.end,
                        };

                        error = error.or_else(|| {
                            Some(ParseError::MissingPositional("block".into(), err_span))
                        });

                        None
                    }
                } else {
                    let err_span = Span {
                        start: kw_span.end,
                        end: kw_span.end,
                    };

                    error = error.or_else(|| {
                        Some(ParseError::MissingPositional(
                            "environment variable name".into(),
                            err_span,
                        ))
                    });

                    None
                }
            }
            _ => {
                error = error.or_else(|| {
                    Some(ParseError::Expected(
                        // TODO: Fill in more keywords as they come
                        "def or env keyword".into(),
                        spans[1],
                    ))
                });

                None
            }
        }
    } else {
        error = error.or_else(|| {
            Some(ParseError::MissingPositional(
                "def or env keyword".into(), // TODO: keep filling more keywords as they come
                Span {
                    start: export_span.end,
                    end: export_span.end,
                },
            ))
        });

        None
    };

    (
        Statement::Pipeline(Pipeline::from_vec(vec![Expression {
            expr: Expr::Call(call),
            span: span(spans),
            ty: Type::Unknown,
            custom_completion: None,
        }])),
        exportable,
        error,
    )
}

pub fn parse_module_block(
    working_set: &mut StateWorkingSet,
    span: Span,
) -> (Block, Overlay, Option<ParseError>) {
    let mut error = None;

    working_set.enter_scope();

    let source = working_set.get_span_contents(span);

    let (output, err) = lex(source, span.start, &[], &[], true);
    error = error.or(err);

    let (output, err) = lite_parse(&output);
    error = error.or(err);

    for pipeline in &output.block {
        // TODO: Should we add export env predecls as well?
        if pipeline.commands.len() == 1 {
            parse_def_predecl(working_set, &pipeline.commands[0].parts);
        }
    }

    let mut overlay = Overlay::new();

    let block: Block = output
        .block
        .iter()
        .map(|pipeline| {
            if pipeline.commands.len() == 1 {
                let name = working_set.get_span_contents(pipeline.commands[0].parts[0]);

                let (stmt, err) = match name {
                    b"def" => {
                        let (stmt, err) = parse_def(working_set, &pipeline.commands[0].parts);

                        (stmt, err)
                    }
                    // TODO: Currently, it is not possible to define a private env var.
                    // TODO: Exported env vars are usable iside the module only if correctly
                    // exported by the user. For example:
                    //
                    //   > module foo { export env a { "2" }; export def b [] { $nu.env.a } }
                    //
                    // will work only if you call `use foo *; b` but not with `use foo; foo b`
                    // since in the second case, the name of the env var would be $nu.env."foo a".
                    b"export" => {
                        let (stmt, exportable, err) =
                            parse_export(working_set, &pipeline.commands[0].parts);

                        if err.is_none() {
                            let name_span = pipeline.commands[0].parts[2];
                            let name = working_set.get_span_contents(name_span);
                            let name = trim_quotes(name);

                            match exportable {
                                Some(Exportable::Decl(decl_id)) => {
                                    overlay.add_decl(name, decl_id);
                                }
                                Some(Exportable::EnvVar(block_id)) => {
                                    overlay.add_env_var(name, block_id);
                                }
                                None => {} // None should always come with error from parse_export()
                            }
                        }

                        (stmt, err)
                    }
                    _ => (
                        garbage_statement(&pipeline.commands[0].parts),
                        Some(ParseError::UnexpectedKeyword(
                            "expected def or export keyword".into(),
                            pipeline.commands[0].parts[0],
                        )),
                    ),
                };

                if error.is_none() {
                    error = err;
                }

                stmt
            } else {
                error = Some(ParseError::Expected("not a pipeline".into(), span));
                garbage_statement(&[span])
            }
        })
        .into();

    working_set.exit_scope();

    (block, overlay, error)
}

pub fn parse_module(
    working_set: &mut StateWorkingSet,
    spans: &[Span],
) -> (Statement, Option<ParseError>) {
    // TODO: Currently, module is closing over its parent scope (i.e., defs in the parent scope are
    // visible and usable in this module's scope). We want to disable that for files.

    let mut error = None;
    let bytes = working_set.get_span_contents(spans[0]);

    if bytes == b"module" && spans.len() >= 3 {
        let (module_name_expr, err) = parse_string(working_set, spans[1]);
        error = error.or(err);

        let module_name = module_name_expr
            .as_string()
            .expect("internal error: module name is not a string");

        let block_span = spans[2];
        let block_bytes = working_set.get_span_contents(block_span);
        let mut start = block_span.start;
        let mut end = block_span.end;

        if block_bytes.starts_with(b"{") {
            start += 1;
        } else {
            return (
                garbage_statement(spans),
                Some(ParseError::Expected("block".into(), block_span)),
            );
        }

        if block_bytes.ends_with(b"}") {
            end -= 1;
        } else {
            error =
                error.or_else(|| Some(ParseError::Unclosed("}".into(), Span { start: end, end })));
        }

        let block_span = Span { start, end };

        let (block, overlay, err) = parse_module_block(working_set, block_span);
        error = error.or(err);

        let block_id = working_set.add_block(block);
        let _ = working_set.add_overlay(&module_name, overlay);

        let block_expr = Expression {
            expr: Expr::Block(block_id),
            span: block_span,
            ty: Type::Block,
            custom_completion: None,
        };

        let module_decl_id = working_set
            .find_decl(b"module")
            .expect("internal error: missing module command");

        let call = Box::new(Call {
            head: spans[0],
            decl_id: module_decl_id,
            positional: vec![module_name_expr, block_expr],
            named: vec![],
        });

        (
            Statement::Pipeline(Pipeline::from_vec(vec![Expression {
                expr: Expr::Call(call),
                span: span(spans),
                ty: Type::Unknown,
                custom_completion: None,
            }])),
            error,
        )
    } else {
        (
            garbage_statement(spans),
            Some(ParseError::UnknownState(
                "Expected structure: module <name> {}".into(),
                span(spans),
            )),
        )
    }
}

pub fn parse_use(
    working_set: &mut StateWorkingSet,
    spans: &[Span],
) -> (Statement, Option<ParseError>) {
    let mut error = None;
    let bytes = working_set.get_span_contents(spans[0]);

    if bytes == b"use" && spans.len() >= 2 {
        for span in spans[1..].iter() {
            let (_, err) = parse_string(working_set, *span);
            error = error.or(err);
        }

        // TODO: Add checking for importing too long import patterns, e.g.:
        // > use spam foo non existent names here do not throw error
        let (import_pattern, err) = parse_import_pattern(working_set, &spans[1..]);
        error = error.or(err);

        let (import_pattern, overlay) =
            if let Some(overlay_id) = working_set.find_overlay(&import_pattern.head.name) {
                (import_pattern, working_set.get_overlay(overlay_id).clone())
            } else {
                // TODO: Do not close over when loading module from file
                // It could be a file
                if let Ok(module_filename) = String::from_utf8(import_pattern.head.name) {
                    if let Ok(module_path) = canonicalize(&module_filename) {
                        let module_name = if let Some(stem) = module_path.file_stem() {
                            stem.to_string_lossy().to_string()
                        } else {
                            return (
                                garbage_statement(spans),
                                Some(ParseError::ModuleNotFound(spans[1])),
                            );
                        };

                        if let Ok(contents) = std::fs::read(module_path) {
                            let span_start = working_set.next_span_start();
                            working_set.add_file(module_filename, &contents);
                            let span_end = working_set.next_span_start();

                            let (block, overlay, err) =
                                parse_module_block(working_set, Span::new(span_start, span_end));
                            error = error.or(err);

                            let _ = working_set.add_block(block);
                            let _ = working_set.add_overlay(&module_name, overlay.clone());

                            (
                                ImportPattern {
                                    head: ImportPatternHead {
                                        name: module_name.into(),
                                        span: spans[1],
                                    },
                                    members: import_pattern.members,
                                    hidden: HashSet::new(),
                                },
                                overlay,
                            )
                        } else {
                            return (
                                garbage_statement(spans),
                                Some(ParseError::ModuleNotFound(spans[1])),
                            );
                        }
                    } else {
                        error = error.or(Some(ParseError::FileNotFound(
                            module_filename,
                            import_pattern.head.span,
                        )));
                        (ImportPattern::new(), Overlay::new())
                    }
                } else {
                    return (
                        garbage_statement(spans),
                        Some(ParseError::NonUtf8(spans[1])),
                    );
                }
            };

        let decls_to_use = if import_pattern.members.is_empty() {
            overlay.decls_with_head(&import_pattern.head.name)
        } else {
            match &import_pattern.members[0] {
                ImportPatternMember::Glob { .. } => overlay.decls(),
                ImportPatternMember::Name { name, span } => {
                    let mut output = vec![];

                    if let Some(id) = overlay.get_decl_id(name) {
                        output.push((name.clone(), id));
                    } else if !overlay.has_env_var(name) {
                        error = error.or(Some(ParseError::ExportNotFound(*span)))
                    }

                    output
                }
                ImportPatternMember::List { names } => {
                    let mut output = vec![];

                    for (name, span) in names {
                        if let Some(id) = overlay.get_decl_id(name) {
                            output.push((name.clone(), id));
                        } else if !overlay.has_env_var(name) {
                            error = error.or(Some(ParseError::ExportNotFound(*span)));
                            break;
                        }
                    }

                    output
                }
            }
        };

        // Extend the current scope with the module's overlay
        working_set.use_decls(decls_to_use);

        // Create the Use command call
        let use_decl_id = working_set
            .find_decl(b"use")
            .expect("internal error: missing use command");

        let import_pattern_expr = Expression {
            expr: Expr::ImportPattern(import_pattern),
            span: span(&spans[1..]),
            ty: Type::List(Box::new(Type::String)),
            custom_completion: None,
        };

        let call = Box::new(Call {
            head: spans[0],
            decl_id: use_decl_id,
            positional: vec![import_pattern_expr],
            named: vec![],
        });

        (
            Statement::Pipeline(Pipeline::from_vec(vec![Expression {
                expr: Expr::Call(call),
                span: span(spans),
                ty: Type::Unknown,
                custom_completion: None,
            }])),
            error,
        )
    } else {
        (
            garbage_statement(spans),
            Some(ParseError::UnknownState(
                "Expected structure: use <name>".into(),
                span(spans),
            )),
        )
    }
}

pub fn parse_hide(
    working_set: &mut StateWorkingSet,
    spans: &[Span],
) -> (Statement, Option<ParseError>) {
    let mut error = None;
    let bytes = working_set.get_span_contents(spans[0]);

    if bytes == b"hide" && spans.len() >= 2 {
        for span in spans[1..].iter() {
            let (_, err) = parse_string(working_set, *span);
            error = error.or(err);
        }

        let (import_pattern, err) = parse_import_pattern(working_set, &spans[1..]);
        error = error.or(err);

        let (is_module, overlay) =
            if let Some(overlay_id) = working_set.find_overlay(&import_pattern.head.name) {
                (true, working_set.get_overlay(overlay_id).clone())
            } else if import_pattern.members.is_empty() {
                // The pattern head can be e.g. a function name, not just a module
                if let Some(id) = working_set.find_decl(&import_pattern.head.name) {
                    let mut decls = HashMap::new();
                    decls.insert(import_pattern.head.name.clone(), id);

                    (
                        false,
                        Overlay {
                            decls,
                            env_vars: HashMap::new(),
                        },
                    )
                } else {
                    // Or it could be an env var
                    (
                        false,
                        Overlay {
                            decls: HashMap::new(),
                            env_vars: HashMap::new(),
                        },
                    )
                }
            } else {
                return (
                    garbage_statement(spans),
                    Some(ParseError::ModuleNotFound(spans[1])),
                );
            };

        // This kind of inverts the import pattern matching found in parse_use()
        let decls_to_hide = if import_pattern.members.is_empty() {
            if is_module {
                overlay.decls_with_head(&import_pattern.head.name)
            } else {
                overlay.decls()
            }
        } else {
            match &import_pattern.members[0] {
                ImportPatternMember::Glob { .. } => overlay.decls(),
                ImportPatternMember::Name { name, span } => {
                    let mut output = vec![];

                    if let Some(item) = overlay.decl_with_head(name, &import_pattern.head.name) {
                        output.push(item);
                    } else if !overlay.has_env_var(name) {
                        error = error.or(Some(ParseError::ExportNotFound(*span)));
                    }

                    output
                }
                ImportPatternMember::List { names } => {
                    let mut output = vec![];

                    for (name, span) in names {
                        if let Some(item) = overlay.decl_with_head(name, &import_pattern.head.name)
                        {
                            output.push(item);
                        } else if !overlay.has_env_var(name) {
                            error = error.or(Some(ParseError::ExportNotFound(*span)));
                            break;
                        }
                    }

                    output
                }
            }
        };

        // TODO: `use spam; use spam foo; hide foo` will hide both `foo` and `spam foo` since
        // they point to the same DeclId. Do we want to keep it that way?
        working_set.hide_decls(&decls_to_hide);
        let import_pattern = import_pattern
            .with_hidden(decls_to_hide.iter().map(|(name, _)| name.clone()).collect());

        // Create the Hide command call
        let hide_decl_id = working_set
            .find_decl(b"hide")
            .expect("internal error: missing hide command");

        let import_pattern_expr = Expression {
            expr: Expr::ImportPattern(import_pattern),
            span: span(&spans[1..]),
            ty: Type::List(Box::new(Type::String)),
            custom_completion: None,
        };

        let call = Box::new(Call {
            head: spans[0],
            decl_id: hide_decl_id,
            positional: vec![import_pattern_expr],
            named: vec![],
        });

        (
            Statement::Pipeline(Pipeline::from_vec(vec![Expression {
                expr: Expr::Call(call),
                span: span(spans),
                ty: Type::Unknown,
                custom_completion: None,
            }])),
            error,
        )
    } else {
        (
            garbage_statement(spans),
            Some(ParseError::UnknownState(
                "Expected structure: hide <name>".into(),
                span(spans),
            )),
        )
    }
}

pub fn parse_let(
    working_set: &mut StateWorkingSet,
    spans: &[Span],
) -> (Statement, Option<ParseError>) {
    let name = working_set.get_span_contents(spans[0]);

    if name == b"let" {
        if let Some((span, err)) = check_name(working_set, spans) {
            return (
                Statement::Pipeline(Pipeline::from_vec(vec![garbage(*span)])),
                Some(err),
            );
        }

        if let Some(decl_id) = working_set.find_decl(b"let") {
            if spans.len() >= 4 {
                // This is a bit of by-hand parsing to get around the issue where we want to parse in the reverse order
                // so that the var-id created by the variable isn't visible in the expression that init it
                for span in spans.iter().enumerate() {
                    let item = working_set.get_span_contents(*span.1);
                    if item == b"=" && spans.len() > (span.0 + 1) {
                        let mut error = None;

                        let mut idx = span.0;
                        let (rvalue, err) = parse_multispan_value(
                            working_set,
                            spans,
                            &mut idx,
                            &SyntaxShape::Keyword(b"=".to_vec(), Box::new(SyntaxShape::Expression)),
                        );
                        error = error.or(err);

                        let mut idx = 0;
                        let (lvalue, err) =
                            parse_var_with_opt_type(working_set, &spans[1..(span.0)], &mut idx);
                        error = error.or(err);

                        let var_id = lvalue.as_var();

                        let rhs_type = rvalue.ty.clone();

                        if let Some(var_id) = var_id {
                            if var_id != CONFIG_VARIABLE_ID {
                                working_set.set_variable_type(var_id, rhs_type);
                            }
                        }

                        let call = Box::new(Call {
                            decl_id,
                            head: spans[0],
                            positional: vec![lvalue, rvalue],
                            named: vec![],
                        });

                        return (
                            Statement::Pipeline(Pipeline::from_vec(vec![Expression {
                                expr: Expr::Call(call),
                                span: nu_protocol::span(spans),
                                ty: Type::Unknown,
                                custom_completion: None,
                            }])),
                            error,
                        );
                    }
                }
            }
            let (call, err) = parse_internal_call(working_set, spans[0], &spans[1..], decl_id);

            return (
                Statement::Pipeline(Pipeline {
                    expressions: vec![Expression {
                        expr: Expr::Call(call),
                        span: nu_protocol::span(spans),
                        ty: Type::Unknown,
                        custom_completion: None,
                    }],
                }),
                err,
            );
        }
    }
    (
        garbage_statement(spans),
        Some(ParseError::UnknownState(
            "internal error: let statement unparseable".into(),
            span(spans),
        )),
    )
}

pub fn parse_source(
    working_set: &mut StateWorkingSet,
    spans: &[Span],
) -> (Statement, Option<ParseError>) {
    let mut error = None;
    let name = working_set.get_span_contents(spans[0]);

    if name == b"source" {
        if let Some(decl_id) = working_set.find_decl(b"source") {
            // Is this the right call to be using here?
            // Some of the others (`parse_let`) use it, some of them (`parse_hide`) don't.
            let (call, err) = parse_internal_call(working_set, spans[0], &spans[1..], decl_id);
            error = error.or(err);

            // Command and one file name
            if spans.len() >= 2 {
                let name_expr = working_set.get_span_contents(spans[1]);
                if let Ok(filename) = String::from_utf8(name_expr.to_vec()) {
                    if let Ok(path) = canonicalize(&filename) {
                        if let Ok(contents) = std::fs::read(&path) {
                            // This will load the defs from the file into the
                            // working set, if it was a successful parse.
                            let (block, err) = parse(
                                working_set,
                                path.file_name().and_then(|x| x.to_str()),
                                &contents,
                                false,
                            );

                            if err.is_some() {
                                // Unsuccessful parse of file
                                return (
                                    Statement::Pipeline(Pipeline::from_vec(vec![Expression {
                                        expr: Expr::Call(call),
                                        span: span(&spans[1..]),
                                        ty: Type::Unknown,
                                        custom_completion: None,
                                    }])),
                                    // Return the file parse error
                                    err,
                                );
                            } else {
                                // Save the block into the working set
                                let block_id = working_set.add_block(block);

                                let mut call_with_block = call;

                                // Adding this expression to the positional creates a syntax highlighting error
                                // after writing `source example.nu`
                                call_with_block.positional.push(Expression {
                                    expr: Expr::Int(block_id as i64),
                                    span: spans[1],
                                    ty: Type::Unknown,
                                    custom_completion: None,
                                });

                                return (
                                    Statement::Pipeline(Pipeline::from_vec(vec![Expression {
                                        expr: Expr::Call(call_with_block),
                                        span: span(spans),
                                        ty: Type::Unknown,
                                        custom_completion: None,
                                    }])),
                                    None,
                                );
                            }
                        }
                    } else {
                        error = error.or(Some(ParseError::FileNotFound(filename, spans[1])));
                    }
                } else {
                    return (
                        garbage_statement(spans),
                        Some(ParseError::NonUtf8(spans[1])),
                    );
                }
            }
            return (
                Statement::Pipeline(Pipeline::from_vec(vec![Expression {
                    expr: Expr::Call(call),
                    span: span(spans),
                    ty: Type::Unknown,
                    custom_completion: None,
                }])),
                error,
            );
        }
    }
    (
        garbage_statement(spans),
        Some(ParseError::UnknownState(
            "internal error: source statement unparseable".into(),
            span(spans),
        )),
    )
}

#[cfg(feature = "plugin")]
pub fn parse_register(
    working_set: &mut StateWorkingSet,
    spans: &[Span],
) -> (Statement, Option<ParseError>) {
    use crate::parser::check_call;
    use nu_plugin::{get_signature, EncodingType, PluginDeclaration};
    use nu_protocol::Signature;

    // Checking that the function is used with the correct name
    // Maybe this is not necessary but it is a sanity check
    if working_set.get_span_contents(spans[0]) != b"register" {
        return (
            garbage_statement(spans),
            Some(ParseError::UnknownState(
                "internal error: Wrong call name for parse plugin function".into(),
                span(spans),
            )),
        );
    }

    // Parsing the spans and checking that they match the register signature
    // Using a parsed call makes more sense than checking for how many spans are in the call
    // Also, by creating a call, it can be checked if it matches the declaration signature
    let (call, call_span) = match working_set.find_decl(b"register") {
        None => {
            return (
                garbage_statement(spans),
                Some(ParseError::UnknownState(
                    "internal error: Register declaration not found".into(),
                    span(spans),
                )),
            )
        }
        Some(decl_id) => {
            let (call, mut err) = parse_internal_call(working_set, spans[0], &spans[1..], decl_id);
            let decl = working_set.get_decl(decl_id);

            let call_span = span(spans);

            err = check_call(call_span, &decl.signature(), &call).or(err);
            if err.is_some() || call.has_flag("help") {
                return (
                    Statement::Pipeline(Pipeline::from_vec(vec![Expression {
                        expr: Expr::Call(call),
                        span: call_span,
                        ty: Type::Unknown,
                        custom_completion: None,
                    }])),
                    err,
                );
            }

            (call, call_span)
        }
    };

    // Extracting the required arguments from the call and keeping them together in a tuple
    // The ? operator is not used because the error has to be kept to be printed in the shell
    // For that reason the values are kept in a result that will be passed at the end of this call
    let arguments = call
        .positional
        .get(0)
        .map(|expr| {
            let name_expr = working_set.get_span_contents(expr.span);
            String::from_utf8(name_expr.to_vec())
                .map_err(|_| ParseError::NonUtf8(expr.span))
                .and_then(|name| {
                    canonicalize(&name).map_err(|_| ParseError::FileNotFound(name, expr.span))
                })
                .and_then(|path| {
                    if path.exists() & path.is_file() {
                        Ok(path)
                    } else {
                        Err(ParseError::FileNotFound(format!("{:?}", path), expr.span))
                    }
                })
        })
        .expect("required positional has being checked")
        .and_then(|path| {
            call.get_flag_expr("encoding")
                .map(|expr| {
                    EncodingType::try_from_bytes(working_set.get_span_contents(expr.span))
                        .ok_or_else(|| {
                            ParseError::IncorrectValue(
                                "wrong encoding".into(),
                                expr.span,
                                "Encodings available: capnp and json".into(),
                            )
                        })
                })
                .expect("required named has being checked")
                .map(|encoding| (path, encoding))
        });

    // Signature is an optional value from the call and will be used to decide if
    // the plugin is called to get the signatures or to use the given signature
    let signature = call.positional.get(1).map(|expr| {
        let signature = working_set.get_span_contents(expr.span);
        serde_json::from_slice::<Signature>(signature).map_err(|_| {
            ParseError::LabeledError(
                "Signature deserialization error".into(),
                "unable to deserialize signature".into(),
                spans[0],
            )
        })
    });

    // Shell is another optional value used as base to call shell to plugins
    let shell = call.get_flag_expr("shell").map(|expr| {
        let shell_expr = working_set.get_span_contents(expr.span);

        String::from_utf8(shell_expr.to_vec())
            .map_err(|_| ParseError::NonUtf8(expr.span))
            .and_then(|name| {
                canonicalize(&name).map_err(|_| ParseError::FileNotFound(name, expr.span))
            })
            .and_then(|path| {
                if path.exists() & path.is_file() {
                    Ok(path)
                } else {
                    Err(ParseError::FileNotFound(format!("{:?}", path), expr.span))
                }
            })
    });

    let shell = match shell {
        None => None,
        Some(path) => match path {
            Ok(path) => Some(path),
            Err(err) => {
                return (
                    Statement::Pipeline(Pipeline::from_vec(vec![Expression {
                        expr: Expr::Call(call),
                        span: call_span,
                        ty: Type::Unknown,
                        custom_completion: None,
                    }])),
                    Some(err),
                );
            }
        },
    };

    let error = match signature {
        Some(signature) => arguments.and_then(|(path, encoding)| {
            signature.map(|signature| {
                let plugin_decl = PluginDeclaration::new(path, signature, encoding, shell);
                working_set.add_decl(Box::new(plugin_decl));
                working_set.mark_plugins_file_dirty();
            })
        }),
        None => arguments.and_then(|(path, encoding)| {
            get_signature(path.as_path(), &encoding, &shell)
                .map_err(|err| {
                    ParseError::LabeledError(
                        "Error getting signatures".into(),
                        err.to_string(),
                        spans[0],
                    )
                })
                .map(|signatures| {
                    for signature in signatures {
                        // create plugin command declaration (need struct impl Command)
                        // store declaration in working set
                        let plugin_decl = PluginDeclaration::new(
                            path.clone(),
                            signature,
                            encoding.clone(),
                            shell.clone(),
                        );

                        working_set.add_decl(Box::new(plugin_decl));
                    }

                    working_set.mark_plugins_file_dirty();
                })
        }),
    }
    .err();

    (
        Statement::Pipeline(Pipeline::from_vec(vec![Expression {
            expr: Expr::Call(call),
            span: call_span,
            ty: Type::Nothing,
            custom_completion: None,
        }])),
        error,
    )
}
