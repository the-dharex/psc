use ariadne::{Color, Label, Report, ReportKind, Source};
use chumsky::prelude::*;
use lexer::Token;
use logos::Logos;

pub mod lexer;
pub mod parser;
pub mod ast;
pub mod sema;
pub mod codegen;
pub mod optimizer;

pub fn compile(source: &str, filename: &str) -> Option<ast::Program> {
    // Tokenización — reportar errores léxicos correctamente
    let mut lex_errors: Vec<std::ops::Range<usize>> = Vec::new();
    let tokens: Vec<(Token, std::ops::Range<usize>)> = Token::lexer(source)
        .spanned()
        .filter_map(|(token, span)| match token {
            Ok(t) => Some((t, span)),
            Err(_) => {
                lex_errors.push(span);
                None
            }
        })
        .collect();

    if !lex_errors.is_empty() {
        for span in &lex_errors {
            let fragment = &source[span.clone()];
            Report::build(ReportKind::Error, filename, span.start)
                .with_message(format!("Token no reconocido: '{}'", fragment))
                .with_label(
                    Label::new((filename, span.clone()))
                        .with_message("carácter o secuencia inválida")
                        .with_color(Color::Red),
                )
                .finish()
                .print((filename, Source::from(source)))
                .unwrap();
        }
        return None;
    }

    let token_stream = chumsky::Stream::from_iter(
        source.len()..source.len(),
        tokens.into_iter()
    );

    match parser::parser().parse(token_stream) {
        Ok(mut program) => {
            match sema::analyze(&program) {
                Ok(_symbol_table) => {
                    // Ejecutar optimizador de AST
                    let mut stats = optimizer::OptStats::default();
                    optimizer::optimize(&mut program, &mut stats);
                    Some(program)
                }
                Err(errors) => {
                    for e in errors {
                        eprintln!("\x1b[31mSemantic Error:\x1b[0m {}", e);
                    }
                    None
                }
            }
        },
        Err(parse_errs) => {
            for err in parse_errs {
                Report::build(ReportKind::Error, filename, err.span().start)
                    .with_message(err.to_string())
                    .with_label(
                        Label::new((filename, err.span()))
                            .with_message(format!("{:?}", err.reason()))
                            .with_color(Color::Red),
                    )
                    .finish()
                    .print((filename, Source::from(source)))
                    .unwrap();
            }
            None
        }
    }
}
