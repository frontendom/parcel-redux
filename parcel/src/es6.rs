use std::fmt;
use std::fmt::Write;
use std::borrow::Cow;
use std::collections::HashSet;

use esparse;
use esparse::lex::{self, Tt};
use esparse::skip::{self, Prec};

macro_rules! expected {
    ($lex:expr, $msg:expr) => {{
        return Err(Error {
            kind: ErrorKind::Expected($msg),
            span: $lex.here().span.with_owned(),
        })
    }};
}

#[derive(Debug, PartialEq, Eq)]
pub enum Export<'s> {
    Default(&'s str),
    AllFrom(Cow<'s, str>),
    Named(Vec<ExportSpec<'s>>),
    NamedFrom(Vec<ExportSpec<'s>>, Cow<'s, str>),
}

#[derive(Debug, PartialEq, Eq, Hash)]
pub struct ExportSpec<'s> {
    bind: &'s str,
    name: &'s str,
}

impl<'s> ExportSpec<'s> {
    #[inline]
    pub fn new(bind: &'s str, name: &'s str) -> Self {
        ExportSpec {
            name,
            bind,
        }
    }

    #[inline]
    pub fn same(name: &'s str) -> Self {
        ExportSpec::new(name, name)
    }
}

#[derive(Debug, PartialEq, Eq, Hash)]
pub struct Import<'s> {
    module_source: &'s str,
    module: Cow<'s, str>,
    default_bind: Option<&'s str>,
    binds: Bindings<'s>,
}

impl<'s> Import<'s> {
    #[inline]
    pub fn new(module_source: &'s str, module: Cow<'s, str>) -> Self {
        Import {
            module_source,
            module,
            default_bind: None,
            binds: Bindings::None,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Hash)]
pub enum Bindings<'s> {
    None,
    NameSpace(&'s str),
    Named(Vec<ImportSpec<'s>>),
}

#[derive(Debug, PartialEq, Eq, Hash)]
pub struct ImportSpec<'s> {
    name: &'s str,
    bind: &'s str,
}

impl<'s> ImportSpec<'s> {
    #[inline]
    pub fn new(name: &'s str, bind: &'s str) -> Self {
        ImportSpec {
            name,
            bind,
        }
    }

    #[inline]
    pub fn same(name: &'s str) -> Self {
        ImportSpec::new(name, name)
    }
}

#[derive(Debug)]
pub struct CjsModule<'s> {
    pub source_prefix: String,
    pub source: String,
    pub source_suffix: String,
    pub deps: HashSet<Cow<'s, str>>,
}

pub type Result<T> = ::std::result::Result<T, Error>;

#[derive(Debug)]
pub struct Error {
    pub kind: ErrorKind,
    pub span: esparse::ast::SpanT<String>,
}

#[derive(Debug)]
pub enum ErrorKind {
    Expected(&'static str),
    ParseStrLitError(lex::ParseStrLitError),
}

impl From<skip::Error> for Error {
    fn from(inner: skip::Error) -> Self {
        Error {
            kind: match inner.kind {
                skip::ErrorKind::Expected(s) => ErrorKind::Expected(s),
            },
            span: inner.span,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        writeln!(f,
            "{} at {}",
            self.kind,
            self.span,
        )
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            ErrorKind::Expected(s) => {
                write!(f, "expected {}", s)
            }
            ErrorKind::ParseStrLitError(ref error) => {
                write!(f, "invalid string literal: {}", error)
            }
        }
    }
}

pub fn module_to_cjs<'f, 's>(lex: &mut lex::Lexer<'f, 's>, allow_require: bool) -> Result<CjsModule<'s>> {
    let mut source = String::new();
    let mut deps = HashSet::new();
    let mut imports = Vec::new();
    let mut exports = Vec::new();
    // TODO source map lines won't match up when module string literal contains newlines
    loop {
        eat!(lex => tok { source.push_str(tok.ws_before) },
            Tt::Export => {
                let export = parse_export(lex, &mut source)?;
                exports.push(export);
            },
            Tt::Import => {
                let import = parse_import(lex, &mut source)?;
                imports.push(import);
            },
            Tt::Id("require") if allow_require => {
                let start_pos = tok.span.start.pos;
                eat!(lex,
                    Tt::Lparen => eat!(lex,
                        Tt::StrLitSgl(dep_source) |
                        Tt::StrLitDbl(dep_source) => eat!(lex,
                            Tt::Rparen => {
                                deps.insert(match lex::str_lit_value(dep_source) {
                                    Ok(dep) => dep,
                                    Err(error) => return Err(Error {
                                        kind: ErrorKind::ParseStrLitError(error),
                                        span: tok.span.with_owned(),
                                    }),
                                });
                            },
                            _ => {},
                        ),
                        _ => {},
                    ),
                    _ => {},
                );

                let here = lex.here();
                let end_pos = here.span.start.pos - here.ws_before.len();
                source.push_str(&lex.input()[start_pos..end_pos]);
            },
            Tt::Eof => break,
            _ => {
                let tok = lex.advance();
                write!(source, "{}{}", tok.ws_before, tok.tt).unwrap();
            },
        );
    }

    let is_module = !allow_require || !exports.is_empty() || !imports.is_empty();
    let mut source_prefix = String::new();

    if is_module {
        write!(source_prefix, "Object.defineProperty(exports, '__esModule', {{value: true}})\n").unwrap();
    }

    if !imports.is_empty() {
        write!(source_prefix, "with (function() {{").unwrap();
        for (i, import) in imports.iter().enumerate() {
            write!(source_prefix, "\n  const __module{} = require._esModule({})", i, import.module_source).unwrap();
        }
        write!(source_prefix, "\n  return Object.freeze(Object.create(null, {{\n    [Symbol.toStringTag]: {{value: 'ModuleImports'}},").unwrap();
        for (i, import) in imports.iter().enumerate() {
            if let Some(bind) = import.default_bind {
                write!(
                    source_prefix,
                    "\n    {}: {{get() {{return __module{}.default}}, enumerable: true}},",
                    bind,
                    i,
                ).unwrap();
            }
            match import.binds {
                Bindings::None => {}
                Bindings::NameSpace(bind) => {
                    write!(
                        source_prefix,
                        "\n    {}: {{value: __module{}, enumerable: true}},",
                        bind,
                        i,
                    ).unwrap();
                }
                Bindings::Named(ref specs) => {
                    for spec in specs {
                        write!(
                            source_prefix,
                            "\n    {}: {{get() {{return __module{}.{}}}, enumerable: true}},",
                            spec.bind,
                            i,
                            spec.name,
                        ).unwrap();
                    }
                }
            }
        }
        write!(source_prefix, "\n  }}))\n}}()) ").unwrap();
    }

    write!(source_prefix, "~function() {{").unwrap();

    if is_module {
        write!(source_prefix, "\n'use strict';\n").unwrap();
    }

    if !exports.is_empty() {
        write!(source_prefix, "Object.defineProperties(exports, {{").unwrap();
        for export in &exports {
            match *export {
                Export::Default(bind) => {
                    write!(
                        source_prefix,
                        "\n  default: {{get() {{return {}}}, enumerable: true}},",
                        bind,
                    ).unwrap();
                }
                Export::AllFrom(_) => unimplemented!(),
                Export::Named(ref specs) => {
                    for spec in specs {
                        write!(
                            source_prefix,
                            "\n  {}: {{get() {{return {}}}, enumerable: true}},",
                            spec.name,
                            spec.bind,
                        ).unwrap();
                    }
                }
                Export::NamedFrom(_, _) => unimplemented!(),
            }
        }
        write!(source_prefix, "\n}});\n").unwrap();
    }

    for import in imports {
        deps.insert(import.module);
    }
    Ok(CjsModule {
        source_prefix,
        source,
        source_suffix: "}()".to_owned(),
        deps,
    })
}

#[inline(always)]
fn parse_export<'f, 's>(lex: &mut lex::Lexer<'f, 's>, source: &mut String) -> Result<Export<'s>> {
    eat!(lex => tok { source.push_str(tok.ws_before) },
        Tt::Default => {
            eat!(lex => tok { write!(source, "{}{}", tok.ws_before, tok.tt).unwrap(); },
                Tt::Class => {
                    let name = eat!(lex => tok { write!(source, "{}{}", tok.ws_before, tok.tt).unwrap(); },
                        Tt::Id(name) => name,
                        _ => expected!(lex, "class name"),
                    );
                    Ok(Export::Default(name))
                },
                Tt::Function => {
                    eat!(lex => tok { write!(source, "{}{}", tok.ws_before, tok.tt).unwrap(); },
                        Tt::Star => {},
                        _ => {},
                    );
                    let name = eat!(lex => tok { write!(source, "{}{}", tok.ws_before, tok.tt).unwrap(); },
                        Tt::Id(name) => name,
                        _ => expected!(lex, "function name"),
                    );
                    Ok(Export::Default(name))
                },
                _ => {
                    source.push_str("const __default = ");
                    // skip::expr(lex, Prec::NoComma)?;
                    Ok(Export::Default("__default"))
                },
            )
        },
        Tt::Star => eat!(lex => tok { source.push_str(tok.ws_before) },
            Tt::Id("from") => eat!(lex => tok { source.push_str(tok.ws_before) },
                Tt::StrLitSgl(module) |
                Tt::StrLitDbl(module) => {
                    Ok(Export::AllFrom(match lex::str_lit_value(module) {
                        Ok(module) => module,
                        Err(error) => return Err(Error {
                            kind: ErrorKind::ParseStrLitError(error),
                            span: tok.span.with_owned(),
                        }),
                    }))
                },
                _ => expected!(lex, "module name (string literal)"),
            ),
            _ => expected!(lex, "keyword 'from'"),
        ),
        Tt::Lbrace => {
            let mut exports = Vec::new();
            loop {
                eat!(lex => tok { source.push_str(tok.ws_before) },
                    Tt::Id(_) |
                    Tt::Default => {
                        let bind = tok.tt.as_str();
                        eat!(lex => tok { source.push_str(tok.ws_before) },
                            Tt::Id("as") => eat!(lex => tok { source.push_str(tok.ws_before) },
                                Tt::Id(_) |
                                Tt::Default => {
                                    let name = tok.tt.as_str();
                                    exports.push(ExportSpec::new(bind, name));
                                    eat!(lex => tok { source.push_str(tok.ws_before) },
                                        Tt::Rbrace => break,
                                        Tt::Comma => {},
                                        _ => expected!(lex, "',' or '}'"),
                                    );
                                },
                                _ => expected!(lex, "export name after keyword 'as'"),
                            ),
                            Tt::Rbrace => {
                                exports.push(ExportSpec::same(bind));
                                break
                            },
                            Tt::Comma => {
                                exports.push(ExportSpec::same(bind));
                            },
                            _ => expected!(lex, "',' or '}' or keyword 'as'"),
                        )
                    },
                    Tt::Rbrace => break,
                    _ => expected!(lex, "binding name or '}'"),
                );
            }
            eat!(lex => tok { source.push_str(tok.ws_before) },
                Tt::Id("from") => eat!(lex => tok { source.push_str(tok.ws_before) },
                    Tt::StrLitSgl(module) |
                    Tt::StrLitDbl(module) => {
                        Ok(Export::NamedFrom(exports, match lex::str_lit_value(module) {
                            Ok(module) => module,
                            Err(error) => return Err(Error {
                                kind: ErrorKind::ParseStrLitError(error),
                                span: tok.span.with_owned(),
                            }),
                        }))
                    },
                    _ => expected!(lex, "module name (string literal)"),
                ),
                _ => {
                    Ok(Export::Named(exports))
                },
            )
        },
        Tt::Var |
        Tt::Const |
        Tt::Id("let") => {
            let start_pos = tok.span.start.pos;
            let mut exports = Vec::new();
            loop {
                eat!(lex,
                    Tt::Id(name) => {
                        exports.push(ExportSpec::same(name));
                        eat!(lex,
                            Tt::Eq => {
                                skip::expr(lex, Prec::NoComma)?;
                                eat!(lex,
                                    Tt::Comma => continue,
                                    _ => break,
                                )
                            },
                            Tt::Comma => continue,
                            _ => break,
                        );
                    },
                    // TODO Tt::Lbrace =>
                    // TODO Tt::Lbracket =>
                    _ => expected!(lex, "binding name"),
                );
            }

            let here = lex.here();
            let end_pos = here.span.start.pos - here.ws_before.len();
            source.push_str(&lex.input()[start_pos..end_pos]);

            Ok(Export::Named(exports))
        },
        Tt::Function => {
            let start_pos = tok.span.start.pos;

            eat!(lex,
                Tt::Star => {},
                _ => {},
            );
            let name = eat!(lex,
                Tt::Id(name) => name,
                _ => expected!(lex, "function name"),
            );
            // eat!(lex,
            //     Tt::Lparen => skip::balanced_parens(lex, 1)?,
            //     _ => expected!(lex, "formal parameter list"),
            // );
            // eat!(lex,
            //     Tt::Lbrace => skip::balanced_braces(lex, 1)?,
            //     _ => expected!(lex, "function body"),
            // );

            let here = lex.here();
            let end_pos = here.span.start.pos - here.ws_before.len();
            source.push_str(&lex.input()[start_pos..end_pos]);

            Ok(Export::Named(vec![ExportSpec::same(name)]))
        },
        Tt::Class => {
            let start_pos = tok.span.start.pos;

            let name = eat!(lex,
                Tt::Id(name) => name,
                _ => expected!(lex, "class name"),
            );
            // eat!(lex,
            //     Tt::Extends => skip::expr(lex, Prec::NoComma)?,
            //     _ => {},
            // );
            // eat!(lex,
            //     Tt::Lbrace => skip::balanced_braces(lex, 1)?,
            //     _ => expected!(lex, "class body"),
            // );

            let here = lex.here();
            let end_pos = here.span.start.pos - here.ws_before.len();
            source.push_str(&lex.input()[start_pos..end_pos]);

            Ok(Export::Named(vec![ExportSpec::same(name)]))
        },
        // TODO Tt::Id("async") =>
        _ => expected!(lex, "keyword 'default' or '*' or '{' or declaration"),
    )
}

#[inline(always)]
fn parse_import<'f, 's>(lex: &mut lex::Lexer<'f, 's>, source: &mut String) -> Result<Import<'s>> {
    #[inline(always)]
    fn parse_binds<'f, 's>(lex: &mut lex::Lexer<'f, 's>, source: &mut String, binds: &mut Bindings<'s>, expected: &'static str) -> Result<()> {
        eat!(lex => tok { source.push_str(tok.ws_before) },
            Tt::Star => eat!(lex => tok { source.push_str(tok.ws_before) },
                Tt::Id("as") => eat!(lex => tok { source.push_str(tok.ws_before) },
                    Tt::Id(name_space) => {
                        *binds = Bindings::NameSpace(name_space)
                    },
                    _ => expected!(lex, "name space binding name"),
                ),
                _ => expected!(lex, "keyword 'as'"),
            ),
            Tt::Lbrace => {
                let mut imports = Vec::new();
                loop {
                    eat!(lex => tok { source.push_str(tok.ws_before) },
                        Tt::Id(_) |
                        Tt::Default => {
                            let name = tok.tt.as_str();
                            eat!(lex => tok { source.push_str(tok.ws_before) },
                                Tt::Id("as") => eat!(lex => tok { source.push_str(tok.ws_before) },
                                    // we don't need | Tt::Default here since it is always a binding name
                                    Tt::Id(bind) => {
                                        imports.push(ImportSpec::new(name, bind));
                                        eat!(lex => tok { source.push_str(tok.ws_before) },
                                            Tt::Rbrace => break,
                                            Tt::Comma => {},
                                            _ => expected!(lex, "',' or '}'"),
                                        );
                                    },
                                    _ => expected!(lex, "binding name after keyword 'as'"),
                                ),
                                Tt::Rbrace => {
                                    imports.push(ImportSpec::same(name));
                                    break
                                },
                                Tt::Comma => {
                                    imports.push(ImportSpec::same(name));
                                },
                                _ => expected!(lex, "',' or '}' or keyword 'as'"),
                            )
                        },
                        Tt::Rbrace => break,
                        _ => expected!(lex, "import specifier or '}'"),
                    );
                }
                *binds = Bindings::Named(imports);
            },
            _ => expected!(lex, expected),
        );
        Ok(())
    }

    let mut default_bind = None;
    let mut binds = Bindings::None;

    eat!(lex => tok { source.push_str(tok.ws_before) },
        Tt::StrLitSgl(module_source) |
        Tt::StrLitDbl(module_source) => {
            return Ok(Import::new(module_source, match lex::str_lit_value(module_source) {
                Ok(module) => module,
                Err(error) => return Err(Error {
                    kind: ErrorKind::ParseStrLitError(error),
                    span: tok.span.with_owned(),
                }),
            }))
        },
        Tt::Id(default) => {
            default_bind = Some(default);
            eat!(lex => tok { source.push_str(tok.ws_before) },
                Tt::Comma => parse_binds(lex, source, &mut binds, "bindings")?,
                _ => {},
            );
        },
        _ => parse_binds(lex, source, &mut binds, "module name (string literal) or bindings")?,
    );
    eat!(lex => tok { source.push_str(tok.ws_before) },
        Tt::Id("from") => {},
        _ => expected!(lex, "keyword 'from'"),
    );
    eat!(lex => tok { source.push_str(tok.ws_before) },
        Tt::StrLitSgl(module_source) |
        Tt::StrLitDbl(module_source) => {
            Ok(Import {
                module_source,
                module: match lex::str_lit_value(module_source) {
                    Ok(module) => module,
                    Err(error) => return Err(Error {
                        kind: ErrorKind::ParseStrLitError(error),
                        span: tok.span.with_owned(),
                    }),
                },
                default_bind,
                binds,
            })
        },
        _ => expected!(lex, "module name (string literal)"),
    )
}

#[cfg(test)]
mod test {
    use super::*;
    use esparse::lex;

    macro_rules! assert_export_form {
        ($source:expr, $result:expr, $out:expr $(,)*) => {{
            let mut lexer = lex::Lexer::new_unnamed($source);
            assert_eq!(lexer.advance().tt, Tt::Export);
            let mut output = String::new();
            assert_eq!(parse_export(&mut lexer, &mut output).unwrap(), $result);
            assert_eq!(output, $out);
        }};
    }

    macro_rules! assert_export_form_err {
        ($source:expr $(,)*) => {{
            let mut lexer = lex::Lexer::new_unnamed($source);
            assert_eq!(lexer.advance().tt, Tt::Export);
            let mut output = String::new();
            parse_export(&mut lexer, &mut output).unwrap_err();
        }};
    }

    #[test]
    fn test_export_default() {
        assert_export_form!(
            "export //\ndefault /* comment */ 0 _next",
            Export::Default("__default"),
            // " //\n /* comment */ 0",
            " //\nconst __default = ",
        );
        assert_export_form!(
            "export default class Test {} _next",
            Export::Default("Test"),
            // "  class Test {}",
            "  class Test",
        );
        assert_export_form!(
            "export default function test() {} _next",
            Export::Default("test"),
            // "  function test() {}",
            "  function test",
        );
        assert_export_form!(
            "export default function* testGen() {} _next",
            Export::Default("testGen"),
            // "  function* testGen() {}",
            "  function* testGen",
        );
    }

    #[test]
    fn test_export_default_exprs_panic() {
        assert_export_form_err!("export default class {} _next");
        assert_export_form_err!("export default function() {} _next");
        assert_export_form_err!("export default function*() {} _next");
    }

    #[test]
    fn test_export_binding() {
        assert_export_form!(
            "export var asdf _next",
            Export::Named(vec![ExportSpec::same("asdf")]),
            " var asdf",
        );
        assert_export_form!(
            "export let a = 1, b = (1, 2), c = 3, d = (za, zb) => b, e _next",
            Export::Named(vec![
                ExportSpec::same("a"),
                ExportSpec::same("b"),
                ExportSpec::same("c"),
                ExportSpec::same("d"),
                ExportSpec::same("e"),
            ]),
            " let a = 1, b = (1, 2), c = 3, d = (za, zb) => b, e",
        );
        assert_export_form!(
            "export const j = class A extends B(c, d) {}, k = 1 _next",
            Export::Named(vec![
                ExportSpec::same("j"),
                ExportSpec::same("k"),
            ]),
            " const j = class A extends B(c, d) {}, k = 1",
        );
    }

    #[test]
    fn test_export_hoistable_declaration() {
        assert_export_form!(
            "export class Test2 {} _next",
            Export::Named(vec![ExportSpec::same("Test2")]),
            // " class Test2 {}",
            " class Test2",
        );
        assert_export_form!(
            "export function test2() {} _next",
            Export::Named(vec![ExportSpec::same("test2")]),
            // " function test2() {}",
            " function test2",
        );
        assert_export_form!(
            "export function* testGen2() {} _next",
            Export::Named(vec![ExportSpec::same("testGen2")]),
            // " function* testGen2() {}",
            " function* testGen2",
        );
    }

    #[test]
    fn test_export_list() {
        assert_export_form!(
            "export {va as vaz, vb, something as default} _next",
            Export::Named(vec![
                ExportSpec::new("va", "vaz"),
                ExportSpec::same("vb"),
                ExportSpec::new("something", "default"),
            ]),
            "       ",
        );
    }

    #[test]
    fn test_export_ns_from() {
        assert_export_form!(
            "export * from 'a_module' _next",
            Export::AllFrom(Cow::Borrowed("a_module")),
            "   ",
        );
    }

    #[test]
    fn test_export_list_from() {
        assert_export_form!(
            "export {va as vaz, vb, something as default, default as something_else, default, default as default} from 'a_module' _next",
            Export::NamedFrom(vec![
                ExportSpec::new("va", "vaz"),
                ExportSpec::same("vb"),
                ExportSpec::new("something", "default"),
                ExportSpec::new("default", "something_else"),
                ExportSpec::same("default"),
                ExportSpec::same("default"),
            ], Cow::Borrowed("a_module")),
            "                ",
        );
    }

    macro_rules! assert_import_form {
        ($source:expr, $result:expr, $out:expr $(,)*) => {{
            let mut lexer = lex::Lexer::new_unnamed($source);
            assert_eq!(lexer.advance().tt, Tt::Import);
            let mut output = String::new();
            assert_eq!(parse_import(&mut lexer, &mut output).unwrap(), $result);
            assert_eq!(output, $out);
        }};
    }

    #[test]
    fn test_import_bare() {
        assert_import_form!(
            "import 'a_module' _next",
            Import {
                module_source: "'a_module'",
                module: Cow::Borrowed("a_module"),
                default_bind: None,
                binds: Bindings::None,
            },
            " ",
        );
        assert_import_form!(
            "import \"a_module\" _next",
            Import {
                module_source: "\"a_module\"",
                module: Cow::Borrowed("a_module"),
                default_bind: None,
                binds: Bindings::None,
            },
            " ",
        );
    }

    #[test]
    fn test_import_default() {
        assert_import_form!(
            "import test from 'a_module' _next",
            Import {
                module_source: "'a_module'",
                module: Cow::Borrowed("a_module"),
                default_bind: Some("test"),
                binds: Bindings::None,
            },
            "   ",
        );
        assert_import_form!(
            "import test from \"a_module\" _next",
            Import {
                module_source: "\"a_module\"",
                module: Cow::Borrowed("a_module"),
                default_bind: Some("test"),
                binds: Bindings::None,
            },
            "   ",
        );
    }

    #[test]
    fn test_import_name_space() {
        assert_import_form!(
            "import * as test from 'a_module' _next",
            Import {
                module_source: "'a_module'",
                module: Cow::Borrowed("a_module"),
                default_bind: None,
                binds: Bindings::NameSpace("test"),
            },
            "     ",
        );
        assert_import_form!(
            "import * as test from \"a_module\" _next",
            Import {
                module_source: "\"a_module\"",
                module: Cow::Borrowed("a_module"),
                default_bind: None,
                binds: Bindings::NameSpace("test"),
            },
            "     ",
        );
    }

    #[test]
    fn test_import_named() {
        assert_import_form!(
            "import { } from 'a_module' _next",
            Import {
                module_source: "'a_module'",
                module: Cow::Borrowed("a_module"),
                default_bind: None,
                binds: Bindings::Named(vec![]),
            },
            "    ",
        );
        assert_import_form!(
            "import { } from \"a_module\" _next",
            Import {
                module_source: "\"a_module\"",
                module: Cow::Borrowed("a_module"),
                default_bind: None,
                binds: Bindings::Named(vec![]),
            },
            "    ",
        );
        assert_import_form!(
            "import { name } from 'a_module' _next",
            Import {
                module_source: "'a_module'",
                module: Cow::Borrowed("a_module"),
                default_bind: None,
                binds: Bindings::Named(vec![
                    ImportSpec::same("name"),
                ]),
            },
            "     ",
        );
        assert_import_form!(
            "import { name , } from 'a_module' _next",
            Import {
                module_source: "'a_module'",
                module: Cow::Borrowed("a_module"),
                default_bind: None,
                binds: Bindings::Named(vec![
                    ImportSpec::same("name"),
                ]),
            },
            "      ",
        );
        assert_import_form!(
            "import { name , another } from 'a_module' _next",
            Import {
                module_source: "'a_module'",
                module: Cow::Borrowed("a_module"),
                default_bind: None,
                binds: Bindings::Named(vec![
                    ImportSpec::same("name"),
                    ImportSpec::same("another"),
                ]),
            },
            "       ",
        );
        assert_import_form!(
            "import { name as thing } from 'a_module' _next",
            Import {
                module_source: "'a_module'",
                module: Cow::Borrowed("a_module"),
                default_bind: None,
                binds: Bindings::Named(vec![
                    ImportSpec::new("name", "thing"),
                ]),
            },
            "       ",
        );
        assert_import_form!(
            "import { name as thing , } from 'a_module' _next",
            Import {
                module_source: "'a_module'",
                module: Cow::Borrowed("a_module"),
                default_bind: None,
                binds: Bindings::Named(vec![
                    ImportSpec::new("name", "thing"),
                ]),
            },
            "        ",
        );
        assert_import_form!(
            "import { name as thing , another , third as one } from 'a_module' _next",
            Import {
                module_source: "'a_module'",
                module: Cow::Borrowed("a_module"),
                default_bind: None,
                binds: Bindings::Named(vec![
                    ImportSpec::new("name", "thing"),
                    ImportSpec::same("another"),
                    ImportSpec::new("third", "one"),
                ]),
            },
            "             ",
        );
    }

    #[test]
    fn test_import_default_named() {
        assert_import_form!(
            "import test , { } from 'a_module' _next",
            Import {
                module_source: "'a_module'",
                module: Cow::Borrowed("a_module"),
                default_bind: Some("test"),
                binds: Bindings::Named(vec![]),
            },
            "      ",
        );
        assert_import_form!(
            "import test , { name as thing , another , third as one } from 'a_module' _next",
            Import {
                module_source: "'a_module'",
                module: Cow::Borrowed("a_module"),
                default_bind: Some("test"),
                binds: Bindings::Named(vec![
                    ImportSpec::new("name", "thing"),
                    ImportSpec::same("another"),
                    ImportSpec::new("third", "one"),
                ]),
            },
            "               ",
        );
    }

    #[test]
    fn test_import_default_name_space() {
        assert_import_form!(
            "import def , * as test from 'a_module' _next",
            Import {
                module_source: "'a_module'",
                module: Cow::Borrowed("a_module"),
                default_bind: Some("def"),
                binds: Bindings::NameSpace("test"),
            },
            "       ",
        );
        assert_import_form!(
            "import def , * as test from \"a_module\" _next",
            Import {
                module_source: "\"a_module\"",
                module: Cow::Borrowed("a_module"),
                default_bind: Some("def"),
                binds: Bindings::NameSpace("test"),
            },
            "       ",
        );
    }
}
