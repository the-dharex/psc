
//! Pase de optimización a nivel AST.
//!
//! Se ejecuta antes del codegen y realiza:
//! - Plegado de constantes (2+3 → 5, "a"+"b" aún no)
//! - Propagación de constantes (limitada: solo en código lineal)
//! - Reducción de fuerza (x*2 → x+x, x^2 → x*x)
//! - Eliminación de código muerto (sentencias inalcanzables después de bucles infinitos, ramas vacías)

use crate::ast::*;
use std::collections::HashMap;

/// Estadísticas de las optimizaciones realizadas, para reportes.
#[derive(Default, Debug)]
pub struct OptStats {
    pub constants_folded: usize,
    pub dead_stmts_removed: usize,
    pub strength_reductions: usize,
    pub propagations: usize,
}

impl std::fmt::Display for OptStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "constantes plegadas: {}, código muerto eliminado: {}, reducciones de fuerza: {}, propagaciones: {}",
            self.constants_folded, self.dead_stmts_removed, self.strength_reductions, self.propagations
        )
    }
}

pub fn optimize(program: &mut Program, stats: &mut OptStats) {
    for func in &mut program.functions {
        optimize_block(&mut func.body, stats);
    }
    optimize_block(&mut program.main_body, stats);
}

fn optimize_block(stmts: &mut Vec<Statement>, stats: &mut OptStats) {
    // 1. Optimizar expresiones y sub-bloques de cada sentencia
    for stmt in stmts.iter_mut() {
        optimize_stmt(stmt, stats);
    }

    // 2. Eliminación de código muerto: eliminar sentencias después de bucles infinitos incondicionales
    //    o después de un retorno, y eliminar ramas If vacías
    let original_len = stmts.len();
    eliminate_dead_code(stmts);
    stats.dead_stmts_removed += original_len.saturating_sub(stmts.len());

    // 3. Propagación de constantes (simple: rastrear constantes conocidas en código lineal)
    propagate_constants(stmts, stats);

    // 4. Re-plegar después de la propagación
    for stmt in stmts.iter_mut() {
        fold_stmt_exprs(stmt, stats);
    }
}

fn optimize_stmt(stmt: &mut Statement, stats: &mut OptStats) {
    match stmt {
        Statement::Assign { value, .. } => {
            *value = fold_expr(value.clone(), stats);
            *value = strength_reduce(value.clone(), stats);
        }
        Statement::IndexAssign { indices, value, .. } => {
            for idx in indices.iter_mut() {
                *idx = fold_expr(idx.clone(), stats);
            }
            *value = fold_expr(value.clone(), stats);
            *value = strength_reduce(value.clone(), stats);
        }
        Statement::Write(exprs, _) => {
            for expr in exprs.iter_mut() {
                *expr = fold_expr(expr.clone(), stats);
            }
        }
        Statement::If { condition, then_branch, else_branch } => {
            *condition = fold_expr(condition.clone(), stats);

            // Si la condición es una constante conocida, simplificar
            if let Expression::Literal(Literal::Boolean(val)) = condition {
                let val = *val;
                if val {
                    // Siempre verdadero: reemplazar If con contenido de then_branch
                    let mut body = std::mem::take(then_branch);
                    optimize_block(&mut body, stats);
                    stats.dead_stmts_removed += else_branch.as_ref().map(|e| e.len()).unwrap_or(0);
                    // No podemos reemplazar stmt directamente aquí, al menos optimizar la rama tomada
                    *then_branch = body;
                    *else_branch = None;
                } else {
                    // Siempre falso: reemplazar con else_branch
                    stats.dead_stmts_removed += then_branch.len();
                    then_branch.clear();
                    if let Some(eb) = else_branch {
                        optimize_block(eb, stats);
                    }
                }
            } else {
                optimize_block(then_branch, stats);
                if let Some(else_branch) = else_branch {
                    optimize_block(else_branch, stats);
                }
            }
        }
        Statement::While { condition, body } => {
            *condition = fold_expr(condition.clone(), stats);
            optimize_block(body, stats);
        }
        Statement::Repeat { body, until } => {
            optimize_block(body, stats);
            *until = fold_expr(until.clone(), stats);
        }
        Statement::For { start, end, step, body, .. } => {
            *start = fold_expr(start.clone(), stats);
            *end = fold_expr(end.clone(), stats);
            if let Some(s) = step {
                *s = fold_expr(s.clone(), stats);
            }
            optimize_block(body, stats);
        }
        Statement::Match { expression, cases, default } => {
            *expression = fold_expr(expression.clone(), stats);
            for (case_expr, case_body) in cases.iter_mut() {
                *case_expr = fold_expr(case_expr.clone(), stats);
                optimize_block(case_body, stats);
            }
            if let Some(d) = default {
                optimize_block(d, stats);
            }
        }
        Statement::Call { args, .. } => {
            for arg in args.iter_mut() {
                *arg = fold_expr(arg.clone(), stats);
            }
        }
        Statement::Wait { duration, .. } => {
            *duration = fold_expr(duration.clone(), stats);
        }
        Statement::Dimension { sizes, .. } => {
            for s in sizes.iter_mut() {
                *s = fold_expr(s.clone(), stats);
            }
        }
        _ => {}
    }
}

/// Plegar expresiones en todas las expresiones de una sentencia (usado después de la propagación).
fn fold_stmt_exprs(stmt: &mut Statement, stats: &mut OptStats) {
    match stmt {
        Statement::Assign { value, .. } => {
            *value = fold_expr(value.clone(), stats);
            *value = strength_reduce(value.clone(), stats);
        }
        Statement::IndexAssign { indices, value, .. } => {
            for idx in indices.iter_mut() {
                *idx = fold_expr(idx.clone(), stats);
            }
            *value = fold_expr(value.clone(), stats);
        }
        Statement::Write(exprs, _) => {
            for expr in exprs.iter_mut() {
                *expr = fold_expr(expr.clone(), stats);
            }
        }
        Statement::If { condition, then_branch, else_branch } => {
            *condition = fold_expr(condition.clone(), stats);
            for s in then_branch.iter_mut() { fold_stmt_exprs(s, stats); }
            if let Some(eb) = else_branch {
                for s in eb.iter_mut() { fold_stmt_exprs(s, stats); }
            }
        }
        Statement::While { condition, body } => {
            *condition = fold_expr(condition.clone(), stats);
            for s in body.iter_mut() { fold_stmt_exprs(s, stats); }
        }
        Statement::Repeat { body, until } => {
            for s in body.iter_mut() { fold_stmt_exprs(s, stats); }
            *until = fold_expr(until.clone(), stats);
        }
        Statement::For { start, end, step, body, .. } => {
            *start = fold_expr(start.clone(), stats);
            *end = fold_expr(end.clone(), stats);
            if let Some(s) = step { *s = fold_expr(s.clone(), stats); }
            for s in body.iter_mut() { fold_stmt_exprs(s, stats); }
        }
        _ => {}
    }
}

/// Plegado de constantes: evaluar expresiones conocidas en tiempo de compilación.
fn fold_expr(expr: Expression, stats: &mut OptStats) -> Expression {
    match expr {
        Expression::Binary { left, op, right } => {
            let left = fold_expr(*left, stats);
            let right = fold_expr(*right, stats);

            // Entero op Entero
            if let (Expression::Literal(Literal::Integer(l)), Expression::Literal(Literal::Integer(r))) = (&left, &right) {
                let l = *l; let r = *r;
                let folded = match op {
                    BinaryOp::Add => Some(Expression::Literal(Literal::Integer(l + r))),
                    BinaryOp::Sub => Some(Expression::Literal(Literal::Integer(l - r))),
                    BinaryOp::Mul => Some(Expression::Literal(Literal::Integer(l * r))),
                    BinaryOp::Div => {
                        // La división siempre produce Real en PSeInt
                        if r != 0 {
                            Some(Expression::Literal(Literal::Real(l as f64 / r as f64)))
                        } else {
                            None
                        }
                    }
                    BinaryOp::Mod => {
                        if r != 0 { Some(Expression::Literal(Literal::Integer(l % r))) }
                        else { None }
                    }
                    BinaryOp::Power => Some(Expression::Literal(Literal::Real((l as f64).powf(r as f64)))),
                    BinaryOp::Eq => Some(Expression::Literal(Literal::Boolean(l == r))),
                    BinaryOp::Ne => Some(Expression::Literal(Literal::Boolean(l != r))),
                    BinaryOp::Lt => Some(Expression::Literal(Literal::Boolean(l < r))),
                    BinaryOp::Le => Some(Expression::Literal(Literal::Boolean(l <= r))),
                    BinaryOp::Gt => Some(Expression::Literal(Literal::Boolean(l > r))),
                    BinaryOp::Ge => Some(Expression::Literal(Literal::Boolean(l >= r))),
                    _ => None,
                };
                if let Some(result) = folded {
                    stats.constants_folded += 1;
                    return result;
                }
            }

            // Real op Real (o mixto Entero/Real)
            if let Some((l, r)) = extract_numeric_pair(&left, &right) {
                let folded = match op {
                    BinaryOp::Add => Some(Expression::Literal(Literal::Real(l + r))),
                    BinaryOp::Sub => Some(Expression::Literal(Literal::Real(l - r))),
                    BinaryOp::Mul => Some(Expression::Literal(Literal::Real(l * r))),
                    BinaryOp::Div => {
                        if r != 0.0 { Some(Expression::Literal(Literal::Real(l / r))) }
                        else { None }
                    }
                    BinaryOp::Power => Some(Expression::Literal(Literal::Real(l.powf(r)))),
                    BinaryOp::Eq => Some(Expression::Literal(Literal::Boolean(l == r))),
                    BinaryOp::Ne => Some(Expression::Literal(Literal::Boolean(l != r))),
                    BinaryOp::Lt => Some(Expression::Literal(Literal::Boolean(l < r))),
                    BinaryOp::Le => Some(Expression::Literal(Literal::Boolean(l <= r))),
                    BinaryOp::Gt => Some(Expression::Literal(Literal::Boolean(l > r))),
                    BinaryOp::Ge => Some(Expression::Literal(Literal::Boolean(l >= r))),
                    _ => None,
                };
                if let Some(result) = folded {
                    stats.constants_folded += 1;
                    return result;
                }
            }

            // Booleano op Booleano
            if let (Expression::Literal(Literal::Boolean(l)), Expression::Literal(Literal::Boolean(r))) = (&left, &right) {
                let l = *l; let r = *r;
                let folded = match op {
                    BinaryOp::And => Some(Expression::Literal(Literal::Boolean(l && r))),
                    BinaryOp::Or => Some(Expression::Literal(Literal::Boolean(l || r))),
                    BinaryOp::Eq => Some(Expression::Literal(Literal::Boolean(l == r))),
                    BinaryOp::Ne => Some(Expression::Literal(Literal::Boolean(l != r))),
                    _ => None,
                };
                if let Some(result) = folded {
                    stats.constants_folded += 1;
                    return result;
                }
            }

            // Simplificaciones de identidad (no cuentan como plegados, pero son útiles)
            // x + 0 → x, x * 1 → x, x * 0 → 0, x - 0 → x
            match (&op, &left, &right) {
                (BinaryOp::Add, _, Expression::Literal(Literal::Integer(0)))
                | (BinaryOp::Sub, _, Expression::Literal(Literal::Integer(0))) => return left,
                (BinaryOp::Add, Expression::Literal(Literal::Integer(0)), _) => return right,
                (BinaryOp::Mul, _, Expression::Literal(Literal::Integer(1))) => return left,
                (BinaryOp::Mul, Expression::Literal(Literal::Integer(1)), _) => return right,
                (BinaryOp::Mul, _, Expression::Literal(Literal::Integer(0)))
                | (BinaryOp::Mul, Expression::Literal(Literal::Integer(0)), _) => {
                    return Expression::Literal(Literal::Integer(0));
                }
                _ => {}
            }

            Expression::Binary {
                left: Box::new(left),
                op,
                right: Box::new(right),
            }
        }
        Expression::Unary { op, expr } => {
            let inner = fold_expr(*expr, stats);
            match (&op, &inner) {
                (UnaryOp::Neg, Expression::Literal(Literal::Integer(n))) => {
                    stats.constants_folded += 1;
                    Expression::Literal(Literal::Integer(-n))
                }
                (UnaryOp::Neg, Expression::Literal(Literal::Real(f))) => {
                    stats.constants_folded += 1;
                    Expression::Literal(Literal::Real(-f))
                }
                (UnaryOp::Not, Expression::Literal(Literal::Boolean(b))) => {
                    stats.constants_folded += 1;
                    Expression::Literal(Literal::Boolean(!b))
                }
                // Doble negación: --x → x
                (UnaryOp::Neg, Expression::Unary { op: UnaryOp::Neg, expr: inner2 }) => {
                    stats.constants_folded += 1;
                    *inner2.clone()
                }
                (UnaryOp::Not, Expression::Unary { op: UnaryOp::Not, expr: inner2 }) => {
                    stats.constants_folded += 1;
                    *inner2.clone()
                }
                _ => Expression::Unary { op, expr: Box::new(inner) },
            }
        }
        Expression::Call { function, args } => {
            let args: Vec<_> = args.into_iter().map(|a| fold_expr(a, stats)).collect();
            Expression::Call { function, args }
        }
        Expression::Index { array, indices } => {
            let indices: Vec<_> = indices.into_iter().map(|i| fold_expr(i, stats)).collect();
            Expression::Index { array, indices }
        }
        other => other,
    }
}

/// Extraer un par de valores f64 si al menos un lado es Real.
/// Solo pliega si al menos uno era Real (entero puro se maneja por separado)
fn extract_numeric_pair(left: &Expression, right: &Expression) -> Option<(f64, f64)> {
    let l = match left {
        Expression::Literal(Literal::Real(f)) => Some(*f),
        Expression::Literal(Literal::Integer(i)) => Some(*i as f64),
        _ => None,
    }?;
    let r = match right {
        Expression::Literal(Literal::Real(f)) => Some(*f),
        Expression::Literal(Literal::Integer(i)) => Some(*i as f64),
        _ => None,
    }?;
    // Solo plegar si al menos uno era Real (entero puro se maneja por separado)
    if matches!(left, Expression::Literal(Literal::Real(_))) || matches!(right, Expression::Literal(Literal::Real(_))) {
        Some((l, r))
    } else {
        None
    }
}

/// Reducción de fuerza: reemplazar operaciones costosas con más baratas.
fn strength_reduce(expr: Expression, stats: &mut OptStats) -> Expression {
    match expr {
        // x ^ 2 → x * x
        Expression::Binary { ref left, op: BinaryOp::Power, ref right } => {
            if let Expression::Literal(Literal::Integer(2)) = right.as_ref() {
                stats.strength_reductions += 1;
                return Expression::Binary {
                    left: left.clone(),
                    op: BinaryOp::Mul,
                    right: left.clone(),
                };
            }
            // x ^ 0 → 1 (como Real, ya que Power retorna Real)
            if let Expression::Literal(Literal::Integer(0)) = right.as_ref() {
                stats.strength_reductions += 1;
                return Expression::Literal(Literal::Real(1.0));
            }
            // x ^ 1 → x
            if let Expression::Literal(Literal::Integer(1)) = right.as_ref() {
                stats.strength_reductions += 1;
                return *left.clone();
            }
            expr
        }
        // x * 2 → x + x
        Expression::Binary { ref left, op: BinaryOp::Mul, ref right } => {
            if let Expression::Literal(Literal::Integer(2)) = right.as_ref() {
                stats.strength_reductions += 1;
                return Expression::Binary {
                    left: left.clone(),
                    op: BinaryOp::Add,
                    right: left.clone(),
                };
            }
            if let Expression::Literal(Literal::Integer(2)) = left.as_ref() {
                stats.strength_reductions += 1;
                return Expression::Binary {
                    left: right.clone(),
                    op: BinaryOp::Add,
                    right: right.clone(),
                };
            }
            expr
        }
        other => other,
    }
}

/// Eliminación de código muerto: eliminar sentencias después de un `Mientras Verdadero`
/// infinito sin interrupción obvia, o después de un patrón de retorno/salida.
fn eliminate_dead_code(stmts: &mut Vec<Statement>) {
    let mut cut_at = None;
    for (i, stmt) in stmts.iter().enumerate() {
        match stmt {
            // Mientras Verdadero Hacer - todo lo posterior es código muerto
            Statement::While { condition: Expression::Literal(Literal::Boolean(true)), .. } => {
                if i + 1 < stmts.len() {
                    cut_at = Some(i + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    if let Some(idx) = cut_at {
        stmts.truncate(idx);
    }

    // Eliminar ramas If vacías donde la condición se plegó a constante
    stmts.retain(|stmt| {
        !matches!(stmt, Statement::If {
            condition: Expression::Literal(Literal::Boolean(false)),
            then_branch,
            else_branch: None,
            ..
        } if then_branch.is_empty())
    });
}

/// Propagación de constantes simple: rastrear variables asignadas a constantes
/// en código lineal y sustituirlas.
fn propagate_constants(stmts: &mut Vec<Statement>, stats: &mut OptStats) {
    let mut known: HashMap<String, Expression> = HashMap::new();

    for stmt in stmts.iter_mut() {
        // Primero, sustituir constantes conocidas en expresiones de esta sentencia
        substitute_in_stmt(stmt, &known, stats);

        match stmt {
            Statement::Assign { target, value } => {
                if is_constant(value) {
                    known.insert(target.clone(), value.clone());
                } else {
                    // Variable reasignada a no-constante, invalidar
                    known.remove(target);
                }
            }
            // Cualquier flujo de control invalida todas las constantes conocidas (conservador)
            Statement::If { .. } | Statement::While { .. } | Statement::Repeat { .. }
            | Statement::For { .. } | Statement::Match { .. } => {
                known.clear();
            }
            // Leer invalida las variables objetivo
            Statement::Read(targets) => {
                for t in targets {
                    if let Expression::Variable(name) = t {
                        known.remove(name);
                    }
                }
            }
            // Las llamadas a funciones pueden modificar parámetros por referencia
            Statement::Call { .. } => {
                known.clear();
            }
            _ => {}
        }
    }
}

fn is_constant(expr: &Expression) -> bool {
    matches!(expr,
        Expression::Literal(Literal::Integer(_))
        | Expression::Literal(Literal::Real(_))
        | Expression::Literal(Literal::Boolean(_))
        | Expression::Literal(Literal::String(_))
    )
}

fn substitute_in_stmt(stmt: &mut Statement, known: &HashMap<String, Expression>, stats: &mut OptStats) {
    match stmt {
        Statement::Assign { value, .. } => {
            substitute_in_expr(value, known, stats);
        }
        Statement::IndexAssign { indices, value, .. } => {
            for idx in indices.iter_mut() {
                substitute_in_expr(idx, known, stats);
            }
            substitute_in_expr(value, known, stats);
        }
        Statement::Write(exprs, _) => {
            for expr in exprs.iter_mut() {
                substitute_in_expr(expr, known, stats);
            }
        }
        Statement::If { condition, .. } => {
            substitute_in_expr(condition, known, stats);
        }
        Statement::While { condition, .. } => {
            substitute_in_expr(condition, known, stats);
        }
        Statement::Repeat { until, .. } => {
            substitute_in_expr(until, known, stats);
        }
        Statement::For { start, end, step, .. } => {
            substitute_in_expr(start, known, stats);
            substitute_in_expr(end, known, stats);
            if let Some(s) = step {
                substitute_in_expr(s, known, stats);
            }
        }
        Statement::Call { args, .. } => {
            for arg in args.iter_mut() {
                substitute_in_expr(arg, known, stats);
            }
        }
        Statement::Wait { duration, .. } => {
            substitute_in_expr(duration, known, stats);
        }
        _ => {}
    }
}

fn substitute_in_expr(expr: &mut Expression, known: &HashMap<String, Expression>, stats: &mut OptStats) {
    match expr {
        Expression::Variable(name) => {
            if let Some(constant) = known.get(name) {
                *expr = constant.clone();
                stats.propagations += 1;
            }
        }
        Expression::Binary { left, right, .. } => {
            substitute_in_expr(left, known, stats);
            substitute_in_expr(right, known, stats);
        }
        Expression::Unary { expr: inner, .. } => {
            substitute_in_expr(inner, known, stats);
        }
        Expression::Call { args, .. } => {
            for arg in args.iter_mut() {
                substitute_in_expr(arg, known, stats);
            }
        }
        Expression::Index { indices, .. } => {
            for idx in indices.iter_mut() {
                substitute_in_expr(idx, known, stats);
            }
        }
        _ => {}
    }
}
