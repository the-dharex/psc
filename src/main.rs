use clap::Parser;
use psc::lexer::Token;
use logos::Logos;
use ariadne::{Report, ReportKind, Label, Source, Color};
use chumsky::Parser as ChumskyParser; // Evitar conflicto con clap::Parser
use std::time::Instant;

#[derive(Parser, Debug)]
#[command(version, about = "PSeInt JIT Compiler", long_about = None)]
struct Args {
    /// Archivo de entrada a compilar
    #[arg(short, long)]
    input: String,

    /// Generar ejecutable nativo en la ruta especificada (requiere compilador C: gcc, cc o MSVC)
    #[arg(short, long)]
    output: Option<String>,

    /// Nivel de optimización: 0 = ninguna, 1 = básica (solo AST), 2 = completa (AST + Cranelift)
    #[arg(short = 'O', long = "opt-level", default_value_t = 2)]
    opt_level: u8,

    /// Mostrar tiempos de compilación y estadísticas de optimización
    #[arg(long)]
    stats: bool,

    /// Solo compilar, no ejecutar (modo JIT)
    #[arg(long)]
    no_run: bool,
}


fn main() {
    let args = Args::parse();
    let total_start = Instant::now();

    let source = std::fs::read_to_string(&args.input).expect("No se pudo leer el archivo de entrada");
    let filename = &args.input;

    // ── Tokenización (Análisis Léxico) ──
    let t0 = Instant::now();

    let mut lex_errors: Vec<std::ops::Range<usize>> = Vec::new();
    let tokens: Vec<(Token, std::ops::Range<usize>)> = Token::lexer(&source)
        .spanned()
        .filter_map(|(token, span)| match token {
            Ok(t) => Some((t, span)),
            Err(_) => {
                lex_errors.push(span);
                None
            }
        })
        .collect();

    let lex_time = t0.elapsed();

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
                .print((filename, Source::from(source.clone())))
                .unwrap();
        }
        eprintln!("Compilación fallida.");
        std::process::exit(1);
    }

    let token_count = tokens.len();

    // ── Análisis Sintáctico ──
    let t1 = Instant::now();

    let token_stream = chumsky::Stream::from_iter(
        source.len()..source.len(),
        tokens.into_iter()
    );

    let mut program = match psc::parser::parser().parse(token_stream) {
        Ok(p) => p,
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
                    .print((filename, Source::from(source.clone())))
                    .unwrap();
            }
            eprintln!("Compilación fallida.");
            std::process::exit(1);
        }
    };

    let parse_time = t1.elapsed();

    // ── Análisis Semántico ──
    let t2 = Instant::now();

    match psc::sema::analyze(&program) {
        Ok(_sym) => {},
        Err(errors) => {
            for e in errors {
                eprintln!("\x1b[31mSemantic Error:\x1b[0m {}", e);
            }
            eprintln!("Compilación fallida.");
            std::process::exit(1);
        }
    }

    let sema_time = t2.elapsed();

    // ── Optimización AST ──
    let t3 = Instant::now();
    let mut opt_stats = psc::optimizer::OptStats::default();

    if args.opt_level >= 1 {
        psc::optimizer::optimize(&mut program, &mut opt_stats);
    }

    let opt_time = t3.elapsed();

    // ── Generación de Código ──
    let t4 = Instant::now();

    let cranelift_opt = if args.opt_level >= 2 {
        psc::codegen::CraneliftOptLevel::Speed
    } else {
        psc::codegen::CraneliftOptLevel::None
    };

    // Bifurcar: AOT (generar ejecutable) vs JIT (compilar y ejecutar en memoria)
    if let Some(ref output_path) = args.output {
        // ===== Modo AOT: generar ejecutable nativo =====
        let aot = psc::codegen::aot::AotCodeGenerator::with_opt_level(cranelift_opt);
        let obj_bytes = match aot.compile(&program) {
            Ok(bytes) => bytes,
            Err(e) => {
                eprintln!("Codegen Error: {}", e);
                std::process::exit(1);
            }
        };

        let codegen_time = t4.elapsed();

        // Enlazar con el runtime C
        let t5 = Instant::now();
        match psc::codegen::aot::link_executable(&obj_bytes, output_path) {
            Ok(()) => {},
            Err(e) => {
                eprintln!("Link Error: {}", e);
                std::process::exit(1);
            }
        }
        let link_time = t5.elapsed();
        let total_compile = total_start.elapsed();

        // Estadísticas
        if args.stats {
            eprintln!("╔══════════════════════════════════════╗");
            eprintln!("║  Compilación AOT (ejecutable nativo) ║");
            eprintln!("╠══════════════════════════════════════╣");
            eprintln!("║  Tokens:          {:>6}             ║", token_count);
            eprintln!("║  Funciones:       {:>6}             ║", program.functions.len());
            eprintln!("║  Sentencias main: {:>6}             ║", program.main_body.len());
            eprintln!("╠──────────────────────────────────────╣");
            eprintln!("║  Léxico:     {:>10.3?}             ║", lex_time);
            eprintln!("║  Parsing:    {:>10.3?}             ║", parse_time);
            eprintln!("║  Semántico:  {:>10.3?}             ║", sema_time);
            eprintln!("║  Optimizer:  {:>10.3?}             ║", opt_time);
            eprintln!("║  Codegen:    {:>10.3?}             ║", codegen_time);
            eprintln!("║  Enlazado:   {:>10.3?}             ║", link_time);
            eprintln!("║  Total:      {:>10.3?}             ║", total_compile);
            eprintln!("╠──────────────────────────────────────╣");
            if args.opt_level >= 1 {
                eprintln!("║  Opt level: {} (AST{})", args.opt_level,
                    if args.opt_level >= 2 { " + Cranelift" } else { "" });
                eprintln!("║  {}", opt_stats);
            } else {
                eprintln!("║  Optimizaciones desactivadas");
            }
            eprintln!("╚══════════════════════════════════════╝");
        }

        eprintln!("Ejecutable generado: {}", output_path);
        return;
    }

    // ===== Modo JIT: compilar y ejecutar en memoria =====

    // ===== Modo JIT: compilar y ejecutar en memoria =====
    let mut codegen = psc::codegen::CodeGenerator::with_opt_level(cranelift_opt);
    let code_ptr = match codegen.compile(&program) {
        Ok(ptr) => ptr,
        Err(e) => {
            eprintln!("Codegen Error: {}", e);
            std::process::exit(1);
        }
    };

    let codegen_time = t4.elapsed();
    let total_compile = total_start.elapsed();

    // ── Estadísticas ──
    if args.stats {
        eprintln!("╔══════════════════════════════════════╗");
        eprintln!("║       Estadísticas de compilación    ║");
        eprintln!("╠══════════════════════════════════════╣");
        eprintln!("║  Tokens:          {:>6}             ║", token_count);
        eprintln!("║  Funciones:       {:>6}             ║", program.functions.len());
        eprintln!("║  Sentencias main: {:>6}             ║", program.main_body.len());
        eprintln!("╠──────────────────────────────────────╣");
        eprintln!("║  Léxico:     {:>10.3?}             ║", lex_time);
        eprintln!("║  Parsing:    {:>10.3?}             ║", parse_time);
        eprintln!("║  Semántico:  {:>10.3?}             ║", sema_time);
        eprintln!("║  Optimizer:  {:>10.3?}             ║", opt_time);
        eprintln!("║  Codegen:    {:>10.3?}             ║", codegen_time);
        eprintln!("║  Total:      {:>10.3?}             ║", total_compile);
        eprintln!("╠──────────────────────────────────────╣");
        if args.opt_level >= 1 {
            eprintln!("║  Opt level: {} (AST{})", args.opt_level,
                if args.opt_level >= 2 { " + Cranelift" } else { "" });
            eprintln!("║  {}", opt_stats);
        } else {
            eprintln!("║  Optimizaciones desactivadas");
        }
        eprintln!("╚══════════════════════════════════════╝");
    }

    // ── Ejecución ──
    if args.no_run {
        if args.stats {
            eprintln!("(--no-run: ejecución omitida)");
        }
        return;
    }

    let exec_start = Instant::now();
    let code_fn = unsafe { std::mem::transmute::<_, extern "C" fn() -> i32>(code_ptr) };
    let res = code_fn();
    let exec_time = exec_start.elapsed();

    if args.stats {
        eprintln!("Ejecución: {:?}", exec_time);
    }

    println!("Programa terminó con: {}", res);
}
