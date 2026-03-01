
use std::fmt;

#[derive(Debug, Clone)]
pub struct Program {
    pub name: String,
    pub functions: Vec<Function>,
    pub main_body: Vec<Statement>, // Cuerpo principal del "Proceso"
}

#[derive(Debug, Clone)]
pub struct Function {
    pub name: String,
    pub params: Vec<(String, Type, bool)>, // (nombre, tipo, por_referencia)
    pub return_type: Option<Type>,
    pub return_var: Option<String>,
    pub body: Vec<Statement>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    Integer,
    Real,
    Boolean,
    String,
    Array(Box<Type>, Vec<usize>), // tipo de elemento + tamaños de dimensión
    Void, // Para subprocesos
}

#[derive(Debug, Clone)]
pub enum Statement {
    Define {
        vars: Vec<String>,
        ty: Type,
    },
    Assign {
        target: String,
        value: Expression,
    },
    Dimension {
        name: String,
        sizes: Vec<Expression>, // Dimensión arr[N] o arr[N, M]
    },
    IndexAssign {
        array: String,
        indices: Vec<Expression>,
        value: Expression,
    },
    If {
        condition: Expression,
        then_branch: Vec<Statement>,
        else_branch: Option<Vec<Statement>>,
    },
    While {
        condition: Expression,
        body: Vec<Statement>,
    },
    Repeat {
        body: Vec<Statement>,
        until: Expression, // Repetir ... Hasta Que <condición>
    },
    For {
        var: String,
        start: Expression,
        end: Expression,
        step: Option<Expression>, // Con Paso (opcional)
        body: Vec<Statement>,
    },
    Match {
        expression: Expression,
        cases: Vec<(Expression, Vec<Statement>)>, // Casos del Segun ... Hacer
        default: Option<Vec<Statement>>, // De Otro Modo (rama por defecto)
    },
    Call {
        function: String,
        args: Vec<Expression>,
    },
    Read(Vec<Expression>), // Leer a, b, datos[i]
    Write(Vec<Expression>, bool), // Escribir "Hola", x -- bool = agregar salto de línea
    ClearScreen,
    Wait {
        duration: Expression,
        milliseconds: bool, // verdadero = milisegundos, falso = segundos
    },
    WaitKey,
    Return(Option<Expression>),
}

#[derive(Debug, Clone)]
pub enum Expression {
    Binary {
        left: Box<Expression>,
        op: BinaryOp,
        right: Box<Expression>,
    },
    Unary {
        op: UnaryOp,
        expr: Box<Expression>,
    },
    Literal(Literal),
    Variable(String),
    Index {
        array: String,
        indices: Vec<Expression>,
    },
    Call {
        function: String,
        args: Vec<Expression>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum BinaryOp {
    Add, Sub, Mul, Div, Mod, Power,
    Eq, Ne, Lt, Le, Gt, Ge,
    And, Or,
}

#[derive(Debug, Clone, PartialEq)]
pub enum UnaryOp {
    Neg, // -x
    Not, // NO x
}

#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    Integer(i64),
    Real(f64),
    String(String),
    Boolean(bool),
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Type::Integer => write!(f, "Entero"),
            Type::Real => write!(f, "Real"),
            Type::Boolean => write!(f, "Logico"),
            Type::String => write!(f, "Caracter"),
            Type::Void => write!(f, "Void"),
            Type::Array(_, _) => write!(f, "Arreglo"),
        }
    }
}
