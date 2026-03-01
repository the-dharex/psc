use logos::Logos;
use std::fmt;

#[derive(Logos, Clone, Debug, PartialEq, Eq, Hash)]
#[logos(skip r"[ \t\n\f\r]+")] // Saltar espacios en blanco incluyendo retorno de carro
pub enum Token {
    // Palabras clave
    #[token("Proceso", ignore(case))]
    #[token("Algoritmo", ignore(case))]
    Proceso,

    #[token("FinProceso", ignore(case))]
    #[token("FinAlgoritmo", ignore(case))]
    FinProceso,

    #[token("Si", ignore(case))]
    Si,
    #[token("Entonces", ignore(case))]
    Entonces,
    #[token("Sino", ignore(case))]
    Sino,
    #[token("FinSi", ignore(case))]
    FinSi,

    #[token("Segun", ignore(case))]
    Segun,
    #[token("Hacer", ignore(case))]
    Hacer,
    #[token("FinSegun", ignore(case))]
    FinSegun,

    #[token("Para", ignore(case))]
    Para,
    #[token("Hasta", ignore(case))]
    Hasta,
    #[token("Con Paso", ignore(case))]
    #[token("Paso", ignore(case))]
    Paso,
    #[token("FinPara", ignore(case))]
    FinPara,

    #[token("Mientras", ignore(case))]
    Mientras,
    #[token("FinMientras", ignore(case))]
    FinMientras,

    #[token("Repetir", ignore(case))]
    Repetir,
    #[token("Que", ignore(case))] // Para 'Hasta Que'
    Que,

    #[token("Por Referencia", ignore(case))]
    PorReferencia,
    #[token("Por Valor", ignore(case))]
    PorValor,

    #[token("Funcion", ignore(case))]
    #[token("SubProceso", ignore(case))]
    #[token("SubAlgoritmo", ignore(case))]
    Funcion,
    #[token("FinFuncion", ignore(case))]
    #[token("FinSubProceso", ignore(case))]
    #[token("FinSubAlgoritmo", ignore(case))]
    FinFuncion,

    #[token("Escribir", ignore(case))]
    Escribir,
    #[token("Leer", ignore(case))]
    Leer,
    #[token("Definir", ignore(case))]
    Definir,
    #[token("Como", ignore(case))]
    Como,

    #[token("Dimension", ignore(case))]
    Dimension,

    #[token("Borrar Pantalla", ignore(case))]
    BorrarPantalla,

    #[token("Sin Saltar", ignore(case))]
    SinSaltar,

    #[token("Esperar", ignore(case))]
    Esperar,
    #[token("Segundos", ignore(case))]
    #[token("Segundo", ignore(case))]
    Segundo,
    #[token("Milisegundos", ignore(case))]
    #[token("Milisegundo", ignore(case))]
    Milisegundo,

    #[token("Tecla", ignore(case))]
    Tecla,

    // Tipos
    #[token("Entero", ignore(case))]
    Entero,
    #[token("Real", ignore(case))]
    Real,
    #[token("Caracter", ignore(case))]
    Caracter,
    #[token("Logico", ignore(case))]
    Logico,
    #[token("Texto", ignore(case))] // Alias de Caracter, generalmente para cadenas
    #[token("Cadena", ignore(case))]
    Texto,

    // Literales booleanos
    #[token("Verdadero", ignore(case))]
    Verdadero,
    #[token("Falso", ignore(case))]
    Falso,

    // Operadores
    #[token("+")]
    Plus,
    #[token("-")]
    Minus,
    #[token("*")]
    Star,
    #[token("/")]
    Slash,
    #[token("^")]
    Caret,
    #[token("MOD", ignore(case))]
    #[token("%")]
    Mod,

    #[token("=")]
    Eq,
    #[token("<-")]
    #[token(":=")]
    Assign,
    #[token("<")]
    Lt,
    #[token(">")]
    Gt,
    #[token("<=")]
    Le,
    #[token(">=")]
    Ge,
    #[token("<>")]
    Ne,

    #[token("&")]
    #[token("Y", ignore(case))]
    And,
    #[token("|")]
    #[token("O", ignore(case))]
    Or,
    #[token("~")]
    #[token("NO", ignore(case))]
    Not,

    // Delimitadores
    #[token("(")]
    LParen,
    #[token(")")]
    RParen,
    #[token("[")]
    LBracket,
    #[token("]")]
    RBracket,
    #[token(",")]
    Comma,
    #[token(";")]
    Semi,
    #[token(":")]
    Colon,

    // Literales
    #[regex(r#""([^"\\]|\\t|\\u|\\n|\\")*""#, |lex| lex.slice().to_owned())]
    #[regex(r#"'([^'\\]|\\t|\\u|\\n|\\')*'"#, |lex| lex.slice().to_owned())] 
    String(String),

    #[regex(r"[0-9]+", |lex| lex.slice().parse().ok())]
    Int(i64),

    #[regex(r"[0-9]+\.[0-9]+", |lex| lex.slice().to_owned())]
    Float(String),

    #[regex(r"[a-zA-Z_][a-zA-Z0-9_]*", |lex| lex.slice().to_owned(), priority = 0)]
    Ident(String),

    #[regex(r"//.*", logos::skip)] // Comentarios de línea
    Comment,

    // Comentarios de bloque se pueden agregar después si es necesario
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}
